//! Native services for the shared workspace activity pane.
//!
//! The pane itself is [`tilo_desktop::activity::RoomActivityPane`]; this
//! module gives it the device's identity, its private seen frontiers and
//! departures (kept in the user's state directory), live agent dispatch through the session's
//! action broker, managed agent launches, and the native workspace tree as
//! its causal history source.
use crate::shell::automation_host::{
    DocumentShare, FinishedShortcutRun, RunGroup, group_share, ordered_steps, record_run_steps,
    record_task_run_steps,
};
use crate::shell::cold_record_reads::ColdRecordReads;
use crate::shell::collab_broker::AppActionBroker;
use crate::shell::room_records::{LocalRecordReader, RoomRecords};
use crate::shell::state::WorkspaceAuthority as _;
use crate::shell::state::{
    Workspace, WorkspaceId, WorkspaceTaskResource, WorkspaceTaskRoute, WorkspaceTaskStatus,
    WorkspaceTree, WorkspaceTreeHandle,
};
use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;
use std::sync::{OnceLock, mpsc};
use std::time::{Duration, Instant};
use tilo_action_broker::task_runs::EndedTaskRun;
use tilo_desktop::activity::{
    ActivityHost, ActivityTree, WorkspaceDeparture, WorkspaceSeenFrontier,
};
pub use tilo_desktop::activity::{ActivityView, Period, RoomActivityOutcome, RoomScope};
use ui::record::{RecordId, RecordStore};

/// History derivation owns its document cursor, not the live workspace lock.
/// Only the admitted desktop, causal document and native task routing are
/// copied; persistence and mutation authority remain on the original tree.
#[derive(Clone)]
struct ActivityObservation {
    desktop: tilo_desktop_model::DesktopTree,
    doc: automerge::AutoCommit,
    native_rooms: BTreeSet<super::state::RoomId>,
    /// The verified agents of every room on this desktop, read with it.
    roster_agents: Vec<ui::history_source::RosterAgent>,
}

impl ActivityObservation {
    fn capture(handle: &WorkspaceTreeHandle) -> Self {
        let mut tree = handle.write();
        tree.commit_room_change();
        Self {
            desktop: tree.desktop.clone(),
            doc: tree.doc.clone(),
            native_rooms: tree
                .desktop
                .workspaces
                .iter()
                .filter(|workspace| tree.requires_native_owner(workspace.room))
                .map(|workspace| workspace.room)
                .collect(),
            roster_agents: verified_agents_since(&mut tree),
        }
    }
}

/// A room's verified agents and the room document's heads they were read at.
type ReadAgents = (
    Vec<automerge::ChangeHash>,
    Vec<ui::history_source::RosterAgent>,
);

thread_local! {
    /// Each room's verified agents as last read.
    static VERIFIED_AGENTS: std::cell::RefCell<HashMap<super::state::RoomId, ReadAgents>> =
        std::cell::RefCell::new(HashMap::new());
}

/// Every room's verified agents ([`WorkspaceTree::verified_agents`]), read
/// again only for a room whose document moved since: Activity refreshes on
/// a workspace switch, and its governance seldom changes.
fn verified_agents_since(tree: &mut WorkspaceTree) -> Vec<ui::history_source::RosterAgent> {
    let rooms = tree
        .desktop
        .workspaces
        .iter()
        .map(|workspace| workspace.room)
        .collect::<BTreeSet<_>>();
    VERIFIED_AGENTS.with(|cache| {
        let mut cache = cache.borrow_mut();
        cache.retain(|room, _| rooms.contains(room));
        let mut agents = Vec::new();
        for room in rooms {
            let heads = tree
                .room_docs
                .get_mut(&room)
                .map(automerge::AutoCommit::get_heads)
                .unwrap_or_default();
            let current = match cache.get(&room) {
                Some((read_at, known)) if *read_at == heads => known.clone(),
                _ => {
                    let read = tree.verified_agents(room);
                    cache.insert(room, (heads, read.clone()));
                    read
                }
            };
            agents.extend(current);
        }
        agents
    })
}

impl std::ops::Deref for ActivityObservation {
    type Target = tilo_desktop_model::DesktopTree;
    fn deref(&self) -> &Self::Target {
        &self.desktop
    }
}

impl std::ops::DerefMut for ActivityObservation {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.desktop
    }
}

impl ActivityTree for ActivityObservation {
    fn document(&self) -> &automerge::AutoCommit {
        &self.doc
    }
    fn document_mut(&mut self) -> &mut automerge::AutoCommit {
        &mut self.doc
    }
    fn resync_activity_cache(&mut self) -> Result<(), String> {
        tilo_desktop_model::desktop_doc::rebuild(&self.doc, &mut self.desktop);
        Ok(())
    }
    fn roster_agents(&self) -> Vec<ui::history_source::RosterAgent> {
        self.roster_agents.clone()
    }
    fn native_manual_tasks(&self, workspace: WorkspaceId) -> bool {
        self.room_of(workspace)
            .is_some_and(|room| self.native_rooms.contains(&room))
    }
}

/// How long a finished shortcut run waits, on the wall clock, for its changes
/// to reach a document on this device before it is given up.
const SHORTCUT_RUN_SETTLE: Duration = Duration::from_secs(10);

/// How often a waiting run looks again. Between looks nothing is read, and a
/// look reads only the documents whose revision moved since the last one.
const SHORTCUT_RUN_LOOK_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Default)]
pub(crate) struct NativeActivityHost {
    broker: Option<crate::shell::collab_broker::AppActionBroker>,
    review: Option<super::task_review::NativeTaskReview>,
    morning: Option<Rc<super::morning_sources::NativeMorningSources>>,
    /// Agent task runs being undone, and how each ended.
    undos: super::task_undo::TaskUndos,
}

/// A document's revision when a waiting run last looked at it: the heads of
/// a document open here, or the persisted witness of one on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
enum LookedRevision {
    Open(Vec<automerge::ChangeHash>),
    Persisted(u64),
}

/// A run the broker saw end: a shortcut's, or an agent's run of a workspace
/// task. Either stamps its changes with its group, and either is recorded as
/// one run in workspace history, undoable as a unit.
#[derive(Clone, Debug)]
pub(crate) enum EndedRun {
    Shortcut(Box<FinishedShortcutRun>),
    Task(EndedTaskRun),
}

impl EndedRun {
    fn group(&self) -> RunGroup {
        match self {
            Self::Shortcut(finished) => RunGroup::shortcut(finished.report.run),
            Self::Task(ended) => RunGroup::task(&ended.task_id),
        }
    }

    fn room(&self) -> &str {
        match self {
            Self::Shortcut(finished) => &finished.room,
            Self::Task(ended) => &ended.room,
        }
    }

    fn name(&self) -> &str {
        match self {
            Self::Shortcut(finished) => &finished.shortcut.name,
            Self::Task(ended) => &ended.title,
        }
    }

    /// Whether the run may have changed a document, so its changes are
    /// worth waiting for: a shortcut that invoked nothing changed nothing.
    fn may_have_changed(&self) -> bool {
        match self {
            Self::Shortcut(finished) => !finished.report.invocations().is_empty(),
            Self::Task(_) => true,
        }
    }

    /// Record the run in `coordinator` as `steps`; the transaction is left
    /// open for the caller to commit.
    fn record(&self, coordinator: &mut automerge::AutoCommit, steps: &[ui::RunStep]) -> bool {
        match self {
            Self::Shortcut(finished) => record_run_steps(coordinator, finished, steps).is_some(),
            Self::Task(ended) => record_task_run_steps(coordinator, ended, steps).is_some(),
        }
    }
}

/// A run the broker saw end whose history is not recorded yet.
pub(crate) struct PendingShortcutRun {
    finished: EndedRun,
    since: Instant,
    /// What the last look found in each candidate document, under the
    /// revision it was read at. `None` when the revision is unknown, so the
    /// document is read again on the next look.
    looked: HashMap<RecordId, (Option<LookedRevision>, Option<DocumentShare>)>,
}

/// One persisted document a waiting run needs looked at, read off the loop.
struct PersistedLook {
    run: RunGroup,
    id: RecordId,
    /// The revision the last look read it at.
    seen: Option<LookedRevision>,
}

/// What a persisted look found.
struct PersistedFound {
    run: RunGroup,
    id: RecordId,
    revision: Option<LookedRevision>,
    share: PersistedShare,
}

/// A persisted document's share of a run, as a look found it.
enum PersistedShare {
    /// The document did not move since the last look: what it found stands.
    Unchanged,
    /// The document as read now.
    Read(Option<DocumentShare>),
}

impl PendingShortcutRun {
    #[cfg(test)]
    pub(crate) fn new(finished: FinishedShortcutRun, since: Instant) -> Self {
        Self::ended(EndedRun::Shortcut(Box::new(finished)), since)
    }

    pub(crate) fn ended(finished: EndedRun, since: Instant) -> Self {
        Self {
            finished,
            since,
            looked: HashMap::new(),
        }
    }

    /// Look at the run's candidates open in `records`, in memory, and name
    /// the persisted ones for a read off the loop: only documents whose
    /// revision moved since the last look are read again.
    fn look_open(
        &mut self,
        candidates: &BTreeSet<RecordId>,
        open: &BTreeSet<RecordId>,
        records: &RecordStore,
    ) -> Vec<PersistedLook> {
        use automerge::transaction::Transactable as _;
        let run = self.finished.group();
        self.looked.retain(|id, _| candidates.contains(id));
        let mut persisted = Vec::new();
        for id in candidates {
            if !open.contains(id) {
                persisted.push(PersistedLook {
                    run: run.clone(),
                    id: id.clone(),
                    seen: self.looked.get(id).and_then(|(seen, _)| seen.clone()),
                });
                continue;
            }
            let revision = records.read(id, |doc| LookedRevision::Open(doc.base_heads()));
            if revision.is_some()
                && self
                    .looked
                    .get(id)
                    .is_some_and(|(seen, _)| *seen == revision)
            {
                continue;
            }
            let share = records
                .read(id, |doc| group_share(&run, id.as_str(), doc))
                .flatten();
            self.looked.insert(id.clone(), (revision, share));
        }
        persisted
    }

    /// Take in what a persisted look found for this run.
    fn take_persisted(&mut self, found: PersistedFound) {
        match found.share {
            PersistedShare::Read(share) => {
                self.looked.insert(found.id, (found.revision, share));
            }
            PersistedShare::Unchanged => {
                if let Some((seen, _)) = self.looked.get_mut(&found.id) {
                    *seen = found.revision;
                }
            }
        }
    }

    /// The run's steps among the documents looked at so far.
    fn steps(&self) -> Vec<ui::RunStep> {
        ordered_steps(
            self.looked
                .values()
                .filter_map(|(_, share)| share.clone())
                .collect(),
        )
    }
}

/// Read the persisted documents `looks` names: a `stat` each, and the
/// document itself only when it moved since its last look. Blocks on the
/// disk (a record may be up to 16 MiB): never on the compositor loop.
fn read_persisted(reader: &LocalRecordReader, looks: Vec<PersistedLook>) -> Vec<PersistedFound> {
    #[cfg(test)]
    held_looks::hold(&looks);
    looks
        .into_iter()
        .map(|look| {
            let revision = reader.revision(&look.id).map(LookedRevision::Persisted);
            let share = if revision.is_some() && look.seen == revision {
                PersistedShare::Unchanged
            } else {
                PersistedShare::Read(
                    reader
                        .load(&look.id)
                        .and_then(|doc| group_share(&look.run, look.id.as_str(), &doc)),
                )
            };
            PersistedFound {
                run: look.run,
                id: look.id,
                revision,
                share,
            }
        })
        .collect()
}

/// Each room document of each run's room, read for that run's share. Lists
/// and reads the disk: never on the compositor loop.
fn read_room_documents(
    reader: &LocalRecordReader,
    rooms: Vec<(RunGroup, crate::shell::state::RoomId)>,
) -> Vec<PersistedFound> {
    let mut found = Vec::new();
    for (run, room) in rooms {
        for document in reader.room_documents(room) {
            let revision = reader.revision(&document.id).map(LookedRevision::Persisted);
            let share = reader
                .load(&document.id)
                .and_then(|doc| group_share(&run, document.id.as_str(), &doc));
            found.push(PersistedFound {
                run: run.clone(),
                id: document.id,
                revision,
                share: PersistedShare::Read(share),
            });
        }
    }
    found
}

/// Test support: hold the persisted reads of a run until released, as a
/// disk that does not answer would.
#[cfg(test)]
pub(crate) mod held_looks {
    use std::sync::{Mutex, mpsc};

    static HELD: Mutex<Vec<(tilo_automation::RunId, mpsc::Receiver<()>)>> = Mutex::new(Vec::new());

    /// Hold the next persisted read for `run` until the returned sender is
    /// used or dropped.
    pub(crate) fn hold_run(run: tilo_automation::RunId) -> mpsc::Sender<()> {
        let (release, held) = mpsc::channel();
        HELD.lock().unwrap().push((run, held));
        release
    }

    pub(super) fn hold(looks: &[super::PersistedLook]) {
        let mut all = HELD.lock().unwrap();
        let held = all
            .iter()
            .position(|(run, _)| {
                looks
                    .iter()
                    .any(|look| look.run == crate::shell::automation_host::RunGroup::shortcut(*run))
            })
            .map(|index| all.remove(index).1);
        drop(all);
        // Bounded, so a read that regressed onto the test's own thread
        // fails the test instead of hanging it.
        if let Some(held) = held {
            let _ = held.recv_timeout(std::time::Duration::from_secs(30));
        }
    }
}

/// Records each shortcut run the broker finishes as one undoable run in the
/// workspace document, from the documents it changed: those open here and
/// those persisted on this device. Driven by the frame pump, never by paint.
/// Persisted documents are read on a worker, and the workspace tree is
/// locked only to write a run that was found.
#[derive(Default)]
pub(crate) struct ShortcutRunRecorder {
    pending: Vec<PendingShortcutRun>,
    next_look: Option<Instant>,
    /// What a look is reading off the loop, until its reads are in.
    reading: Option<mpsc::Receiver<Vec<PersistedFound>>>,
}

impl ShortcutRunRecorder {
    /// Whether a run waits to be recorded, here or in `broker`. Cheap enough
    /// for every tick.
    pub(crate) fn has_work(&self, broker: Option<&AppActionBroker>) -> bool {
        !self.pending.is_empty()
            || broker.is_some_and(|broker| {
                broker.has_finished_shortcut_runs() || broker.has_ended_task_runs()
            })
    }

    /// Wait for `run` too, as if the broker had just handed it over.
    #[cfg(test)]
    pub(crate) fn push(&mut self, run: PendingShortcutRun) {
        self.pending.push(run);
        self.next_look = None;
    }

    #[cfg(test)]
    pub(crate) fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Whether a look's persisted reads are still running.
    #[cfg(test)]
    pub(crate) fn reading(&self) -> bool {
        self.reading.is_some()
    }

    /// Wait for the persisted reads in flight, so the next pass sees them.
    /// Blocks: tests only.
    #[cfg(test)]
    pub(crate) fn finish_reading(&mut self) {
        if let Some(running) = self.reading.take() {
            let (done, ready) = mpsc::channel();
            let _ = done.send(running.recv().unwrap_or_default());
            self.reading = Some(ready);
        }
    }

    /// Take `broker`'s finished runs and record every waiting run whose
    /// changes reached a document here in the tree `lock_tree` locks,
    /// committing it. A look reads the open documents in memory and hands
    /// the persisted ones to a worker; the runs are judged once those reads
    /// are in, on a later pass. A run with no changes found is given up once
    /// [`SHORTCUT_RUN_SETTLE`] has passed since it finished: it changed
    /// nothing this device can undo. Returns whether the tree was written.
    pub(crate) fn record<T: std::ops::DerefMut<Target = WorkspaceTree>>(
        &mut self,
        broker: Option<&AppActionBroker>,
        lock_tree: impl FnOnce() -> T,
        records: &RecordStore,
        local: &RoomRecords,
        now: Instant,
    ) -> bool {
        if let Some(broker) = broker {
            let taken: Vec<EndedRun> = broker
                .take_finished_shortcut_runs()
                .into_iter()
                .map(|finished| EndedRun::Shortcut(Box::new(finished)))
                .chain(
                    broker
                        .take_ended_task_runs()
                        .into_iter()
                        .map(EndedRun::Task),
                )
                .collect();
            if !taken.is_empty() {
                self.next_look = None;
            }
            for ended in taken {
                // A task run again (retried, resumed) is recorded as it is
                // now: its record replaces the one before.
                let group = ended.group();
                self.pending.retain(|run| run.finished.group() != group);
                self.pending.push(PendingShortcutRun::ended(ended, now));
            }
        }
        let found = match self.reading.take() {
            Some(running) => match running.try_recv() {
                Ok(found) => Some(found),
                Err(mpsc::TryRecvError::Empty) => {
                    // The look is not over until its reads are in.
                    self.reading = Some(running);
                    return false;
                }
                Err(mpsc::TryRecvError::Disconnected) => Some(Vec::new()),
            },
            None => None,
        };
        if self.pending.is_empty() {
            return false;
        }
        if let Some(found) = found {
            for found in found {
                if let Some(run) = self
                    .pending
                    .iter_mut()
                    .find(|run| run.finished.group() == found.run)
                {
                    run.take_persisted(found);
                }
            }
        } else {
            if self.next_look.is_some_and(|at| now < at) {
                return false;
            }
            self.next_look = now.checked_add(SHORTCUT_RUN_LOOK_INTERVAL);
            if self.look(records, local) {
                return false;
            }
        }
        self.judge(lock_tree, now)
    }

    /// Look at every waiting run's documents. Returns whether persisted
    /// ones are being read off the loop, so the runs wait for them.
    fn look(&mut self, records: &RecordStore, local: &RoomRecords) -> bool {
        let open: BTreeSet<RecordId> = records
            .open_records()
            .into_iter()
            .map(|checkout| checkout.id)
            .collect();
        let known = local.known();
        let mut persisted = Vec::new();
        // An app whose content is the room's own document (Notes) keeps it
        // beside its records: the worker lists those of each run's room.
        let mut rooms = Vec::new();
        for run in &mut self.pending {
            let room = crate::shell::state::RoomId::parse(run.finished.room());
            if let Some(room) = room {
                rooms.push((run.finished.group(), room));
            }
            let candidates: BTreeSet<RecordId> = open
                .iter()
                .cloned()
                .chain(
                    known
                        .iter()
                        .filter(|record| room.is_some_and(|room| record.is_in(room)))
                        .map(|record| record.id.clone()),
                )
                .collect();
            persisted.extend(run.look_open(&candidates, &open, records));
        }
        let Some(reader) = local
            .local_reader()
            .filter(|_| !persisted.is_empty() || !rooms.is_empty())
        else {
            return false;
        };
        let (found, running) = mpsc::channel();
        // Made on the loop's thread, so the reads wake the loop they are for.
        let wake = crate::shell::search::invocation_wake::waker();
        let spawned = std::thread::Builder::new()
            .name("tilo-shortcut-look".into())
            .spawn(move || {
                let mut read = read_persisted(&reader, persisted);
                read.extend(read_room_documents(&reader, rooms));
                let _ = found.send(read);
                wake.wake();
            });
        match spawned {
            Ok(_) => {
                self.reading = Some(running);
                true
            }
            Err(error) => {
                tracing::warn!(%error, "shortcut run look did not start; persisted documents wait");
                false
            }
        }
    }

    /// Record each waiting run whose changes were found, locking the tree
    /// only if one was, and give up the runs that settled with nothing.
    fn judge<T: std::ops::DerefMut<Target = WorkspaceTree>>(
        &mut self,
        lock_tree: impl FnOnce() -> T,
        now: Instant,
    ) -> bool {
        let mut lock_tree = Some(lock_tree);
        let mut tree: Option<T> = None;
        let mut recorded = false;
        for run in std::mem::take(&mut self.pending) {
            let steps = run.steps();
            if !steps.is_empty() {
                if tree.is_none()
                    && let Some(lock) = lock_tree.take()
                {
                    tree = Some(lock());
                }
                if let Some(tree) = tree.as_mut()
                    && run.finished.record(&mut tree.doc, &steps)
                {
                    recorded = true;
                    continue;
                }
            }
            let settling = now.saturating_duration_since(run.since) < SHORTCUT_RUN_SETTLE;
            if settling && run.finished.may_have_changed() {
                self.pending.push(run);
            } else {
                tracing::debug!(
                    run = %run.finished.name(),
                    "a run that ended changed no document on this device; not recorded"
                );
            }
        }
        if recorded && let Some(tree) = tree.as_mut() {
            tree.commit_room_change();
        }
        recorded
    }
}

/// `$XDG_STATE_HOME/tilo/activity-history.json`: what this device has seen of
/// each workspace and when it last left one. Private viewer state rewritten on
/// every workspace switch, so it stays out of `settings.json`, whose writes
/// every settings reader treats as a preference change to re-apply.
fn history_path() -> Option<std::path::PathBuf> {
    tilo_paths::state_dir().map(|root| root.join("tilo/activity-history.json"))
}

/// `key`'s per-workspace map from the history file at `path`. A missing or
/// malformed file reads as empty.
#[cfg(test)]
fn load_history_map_at<T: serde::de::DeserializeOwned>(
    path: Option<&std::path::Path>,
    key: &str,
) -> HashMap<u64, T> {
    let doc = path.map(read_history_doc).unwrap_or_default();
    history_map_from(doc.get(key))
}

/// Every map in the history file at `path`. A missing or malformed file
/// reads as empty.
fn read_history_doc(path: &std::path::Path) -> serde_json::Map<String, serde_json::Value> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// The per-workspace map one key of the history file holds.
fn history_map_from<T: serde::de::DeserializeOwned>(
    value: Option<&serde_json::Value>,
) -> HashMap<u64, T> {
    value
        .and_then(|value| value.as_object())
        .into_iter()
        .flat_map(|values| values.iter())
        .filter_map(|(id, value)| {
            Some((
                id.parse().ok()?,
                serde_json::from_value(value.clone()).ok()?,
            ))
        })
        .collect()
}

/// What this process knows each history file holds: the file as read once,
/// overlaid with every map saved since. This process is the file's only
/// writer, so the newest map per key is the one saved last here, whether or
/// not the writer thread has reached the disk with it yet.
#[derive(Default)]
struct KnownHistory {
    /// Whether the file itself was read into `maps`.
    read: bool,
    maps: serde_json::Map<String, serde_json::Value>,
}

static KNOWN_HISTORY: std::sync::Mutex<Option<HashMap<std::path::PathBuf, KnownHistory>>> =
    std::sync::Mutex::new(None);

fn with_known_history<R>(path: &std::path::Path, body: impl FnOnce(&mut KnownHistory) -> R) -> R {
    let mut known = KNOWN_HISTORY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    body(
        known
            .get_or_insert_with(HashMap::new)
            .entry(path.to_path_buf())
            .or_default(),
    )
}

/// `key`'s map as this process last saved it, else as the history file at
/// `path` holds it. Never waits for the writer thread: a save still on its
/// way to disk is served from memory, and the file is read at most once.
fn load_known_history_map<T: serde::de::DeserializeOwned>(
    path: &std::path::Path,
    key: &str,
) -> HashMap<u64, T> {
    if let Some(found) = with_known_history(path, |known| {
        (known.read || known.maps.contains_key(key)).then(|| history_map_from(known.maps.get(key)))
    }) {
        return found;
    }
    // Read outside the lock; a map saved meanwhile is newer than the file's.
    let file = read_history_doc(path);
    with_known_history(path, |known| {
        for (name, values) in file {
            known.maps.entry(name).or_insert(values);
        }
        known.read = true;
        history_map_from(known.maps.get(key))
    })
}

/// Replace `key`'s map in the history file at `path`, keeping its other maps:
/// directory `0700`, file `0600`, written atomically.
#[cfg(test)]
fn save_history_map_at<T: serde::Serialize>(
    path: &std::path::Path,
    key: &str,
    values: &HashMap<u64, T>,
) -> std::io::Result<()> {
    write_history_maps_at(path, std::iter::once((key, history_map_value(values))))
}

/// `values` as the JSON object the history file keeps under one key.
fn history_map_value<T: serde::Serialize>(values: &HashMap<u64, T>) -> serde_json::Value {
    serde_json::Value::Object(
        values
            .iter()
            .filter_map(|(id, value)| {
                serde_json::to_value(value)
                    .ok()
                    .map(|value| (id.to_string(), value))
            })
            .collect(),
    )
}

/// Replace each named map in the history file at `path`, keeping the others,
/// in one atomic write.
fn write_history_maps_at<'a>(
    path: &std::path::Path,
    maps: impl IntoIterator<Item = (&'a str, serde_json::Value)>,
) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let mut doc: serde_json::Map<String, serde_json::Value> = std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    for (key, values) in maps {
        doc.insert(key.into(), values);
    }
    let bytes = serde_json::to_vec(&doc).map_err(std::io::Error::other)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }
    tilo_files::native::atomic::write_with_mode(path, &bytes, 0o600)
}

/// `key`'s map in the device's history file. The palette asks for one on
/// every query, often right after a workspace switch saved the others, so
/// this never waits for that save to reach the disk.
fn load_history_map<T: serde::de::DeserializeOwned>(key: &str) -> HashMap<u64, T> {
    history_path()
        .map(|path| load_known_history_map(&path, key))
        .unwrap_or_default()
}

/// Hand `key`'s map to the history writer. Leaving a workspace saves both
/// maps, and the compositor leaves one inside a frame: an atomic replacement
/// syncs the file and its directory, which held a workspace switch's first
/// frame for 2.1 s on a busy disk. The writer keeps the newest map per key
/// and writes them in one replacement once saves pause
/// ([`HISTORY_SAVE_QUIET`]).
fn save_history_map<T: serde::Serialize>(key: &'static str, values: &HashMap<u64, T>) {
    if let Some(path) = history_path() {
        queue_history_save(path, key, history_map_value(values));
    }
}

/// Queue `values` as `key`'s map in the history file at `path`; returns at
/// once unless the writer thread could not start.
fn queue_history_save(path: std::path::PathBuf, key: &'static str, values: serde_json::Value) {
    with_known_history(&path, |known| {
        known.maps.insert(key.to_owned(), values.clone());
    });
    let request = HistoryWrite::Save { path, key, values };
    let unsent = match history_writer() {
        Some(writer) => writer.send(request).err().map(|error| error.0),
        None => Some(request),
    };
    // No writer thread: write here rather than lose the save.
    if let Some(HistoryWrite::Save { path, key, values }) = unsent {
        write_history_now(&path, [(key, values)]);
    }
}

fn write_history_now<'a>(
    path: &std::path::Path,
    maps: impl IntoIterator<Item = (&'a str, serde_json::Value)>,
) {
    if let Err(error) = write_history_maps_at(path, maps) {
        tracing::warn!(%error, path = %path.display(), "could not persist history state");
    }
}

enum HistoryWrite {
    Save {
        path: std::path::PathBuf,
        key: &'static str,
        values: serde_json::Value,
    },
    /// Answer once every earlier save is on disk.
    Settle(mpsc::SyncSender<()>),
}

static HISTORY_WRITER: OnceLock<Option<mpsc::Sender<HistoryWrite>>> = OnceLock::new();

/// The thread that writes the history file, started by the first save.
/// `None` when it cannot start; saves are then written on the caller's thread.
fn history_writer() -> Option<&'static mpsc::Sender<HistoryWrite>> {
    HISTORY_WRITER
        .get_or_init(|| {
            let (requests, received) = mpsc::channel();
            match std::thread::Builder::new()
                .name("tilo-activity-history".into())
                .spawn(move || write_history_requests(&received))
            {
                Ok(_) => Some(requests),
                Err(error) => {
                    tracing::warn!(%error, "history writer did not start");
                    None
                }
            }
        })
        .as_ref()
}

/// Wait until every history save made so far is on disk. Free while nothing
/// was ever saved.
fn settle_history_writes() {
    #[cfg(test)]
    SETTLES_ON_THIS_THREAD.with(|settles| settles.set(settles.get().saturating_add(1)));
    let Some(Some(writer)) = HISTORY_WRITER.get() else {
        return;
    };
    let (done, wait) = mpsc::sync_channel(1);
    if writer.send(HistoryWrite::Settle(done)).is_ok() {
        let _ = wait.recv();
    }
}

#[cfg(test)]
thread_local! {
    /// How often this thread waited for the history writer.
    static SETTLES_ON_THIS_THREAD: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// How long the history writer waits for another save before writing: a
/// burst of workspace switches becomes one synced write after it. A sync
/// forces a journal commit that every other write on the filesystem waits
/// for, the compositor thread's own included, so one per switch kept landing
/// inside a later switch's frames.
const HISTORY_SAVE_QUIET: Duration = Duration::from_secs(2);

/// The longest a save waits for the burst it is part of to end.
const HISTORY_SAVE_LATEST: Duration = Duration::from_secs(10);

/// The writer thread: gather saves until they pause (or a caller settles),
/// keep the newest map per file and key, and write each file once.
fn write_history_requests(received: &mpsc::Receiver<HistoryWrite>) {
    while let Ok(first) = received.recv() {
        let mut batch = HistoryBatch::default();
        let started = Instant::now();
        batch.take(first);
        while batch.settled.is_empty() {
            let waited = started.elapsed();
            let Some(left) = HISTORY_SAVE_LATEST.checked_sub(waited) else {
                break;
            };
            match received.recv_timeout(HISTORY_SAVE_QUIET.min(left)) {
                Ok(request) => batch.take(request),
                Err(_) => break,
            }
        }
        for request in received.try_iter() {
            batch.take(request);
        }
        for (path, maps) in batch.files {
            write_history_now(&path, maps);
        }
        for done in batch.settled {
            let _ = done.send(());
        }
    }
}

/// The saves and settle requests one write of the history files answers.
#[derive(Default)]
struct HistoryBatch {
    files: Vec<(std::path::PathBuf, Vec<(&'static str, serde_json::Value)>)>,
    settled: Vec<mpsc::SyncSender<()>>,
}

impl HistoryBatch {
    fn take(&mut self, request: HistoryWrite) {
        match request {
            HistoryWrite::Save { path, key, values } => {
                let index = self
                    .files
                    .iter()
                    .position(|(known, _)| *known == path)
                    .unwrap_or_else(|| {
                        self.files.push((path, Vec::new()));
                        self.files.len().saturating_sub(1)
                    });
                if let Some((_, maps)) = self.files.get_mut(index) {
                    maps.retain(|(known, _)| *known != key);
                    maps.push((key, values));
                }
            }
            HistoryWrite::Settle(done) => self.settled.push(done),
        }
    }
}

/// Palette actions use only this device's explicit choices for the active room.
pub(crate) fn morning_source_actions(workspace: &Workspace) -> Vec<(String, String)> {
    morning_source_actions_from(workspace, &load_history_map("morning_sources"))
}

fn morning_source_actions_from(
    workspace: &Workspace,
    choices: &HashMap<u64, tilo_desktop::activity::morning::MorningPreferences>,
) -> Vec<(String, String)> {
    use crate::shell::i18n::t;
    use tilo_desktop::activity::morning::{
        MORNING_DRAFT, MORNING_NEXT, MORNING_OPEN, MORNING_REFRESH, MORNING_REMOVE,
    };
    choices
        .get(&workspace.id.raw())
        .map_or_else(Vec::new, |preferences| {
            preferences
                .for_room(&workspace.room.to_string())
                .iter()
                .flat_map(|source| {
                    let app = crate::shell::launcher::display_name_for_app_id_without_waiting(
                        &source.app,
                    )
                    .unwrap_or_else(|| source.app.clone());
                    [
                        (
                            format!("{MORNING_NEXT}{}.{}", workspace.room, source.key()),
                            t("activity-morning-next"),
                        ),
                        (
                            format!("{MORNING_OPEN}{}.{}", workspace.room, source.key()),
                            format!(
                                "{}: {app} · {}",
                                t("activity-morning-open"),
                                source.resource
                            ),
                        ),
                        (
                            format!("{MORNING_REFRESH}{}.{}", workspace.room, source.key()),
                            format!(
                                "{}: {app} · {}",
                                t("activity-morning-refresh"),
                                source.resource
                            ),
                        ),
                        (
                            format!("{MORNING_DRAFT}{}.{}", workspace.room, source.key()),
                            format!(
                                "{}: {app} · {}",
                                t("activity-morning-draft"),
                                source.resource
                            ),
                        ),
                        (
                            format!("{MORNING_REMOVE}{}.{}", workspace.room, source.key()),
                            format!(
                                "{}: {app} · {}",
                                t("activity-morning-remove"),
                                source.resource
                            ),
                        ),
                    ]
                })
                .collect()
        })
}

impl ActivityHost for NativeActivityHost {
    fn task_review_required(&self, room: &str) -> bool {
        self.review
            .as_ref()
            .is_some_and(|review| review.requires_room(room))
    }
    fn task_review_snapshot(
        &self,
        room: &str,
    ) -> Option<Result<std::sync::Arc<tilo_desktop::activity::TaskReviewSnapshot>, String>> {
        self.review.as_ref().map_or_else(
            || Some(Err("authenticated task owner unavailable".into())),
            |review| review.snapshot(room),
        )
    }
    fn submit_task_review(
        &self,
        request: tilo_desktop::activity::TaskReviewRequest,
    ) -> Result<(), String> {
        if let tilo_desktop::activity::TaskReviewCommand::UndoRun { task_id, steps } =
            &request.command
        {
            // Undone by each changed document's history, as the person at
            // the screen: the task's own state does not change.
            let broker = self
                .broker
                .clone()
                .ok_or("the session's action broker is not running yet")?;
            let session = crate::shell::certified_action_session::CertifiedActionSession::open(
                broker,
                &request.room,
                None,
            )?;
            return self.undos.start(
                session.door(),
                request.invocation,
                task_id.clone(),
                steps.clone(),
            );
        }
        self.review
            .as_ref()
            .ok_or("authenticated task owner unavailable")?
            .submit(request)
    }
    fn poll_task_review(&self, invocation: uuid::Uuid) -> Option<Result<(), String>> {
        match self.undos.poll(invocation) {
            Some(super::task_undo::UndoState::Running) => return None,
            Some(super::task_undo::UndoState::Ended(outcome)) => return Some(outcome),
            None => {}
        }
        self.review.as_ref()?.poll(invocation)
    }

    fn local_user(&self) -> String {
        crate::shell::state::local_user_id()
    }
    fn local_author(&self, authors: &mut ui::AuthorDirectory, actor: &str) -> Option<String> {
        #[cfg(test)]
        crate::shell::test_support::ambient_start("history author identity");
        if authors.register_local_identity(actor) {
            None
        } else {
            let user = self.local_user();
            (!user.is_empty()).then_some(user)
        }
    }
    fn morning_sources_available(&self) -> bool {
        true
    }
    fn morning_payload(
        &self,
        room: &str,
        source: &tilo_desktop::activity::morning::MorningSource,
    ) -> Option<tilo_desktop::activity::morning::MorningPayload> {
        self.morning.as_ref()?.payload(room, source)
    }
    fn submit_morning_command(
        &self,
        room: &str,
        source: &tilo_desktop::activity::morning::MorningSource,
        command: tilo_desktop::activity::morning::MorningCommand,
    ) -> Result<(), String> {
        self.morning
            .as_ref()
            .ok_or_else(|| super::i18n::t("activity-morning-command-unavailable"))?
            .submit(room, source, command)
    }
    fn morning_command_status(
        &self,
        room: &str,
        source: &tilo_desktop::activity::morning::MorningSource,
    ) -> Option<String> {
        self.morning.as_ref()?.command_status(room, source)
    }
    fn load_morning_sources(
        &self,
    ) -> HashMap<u64, tilo_desktop::activity::morning::MorningPreferences> {
        load_history_map("morning_sources")
    }
    fn save_morning_sources(
        &self,
        sources: &HashMap<u64, tilo_desktop::activity::morning::MorningPreferences>,
    ) {
        save_history_map("morning_sources", sources);
    }
    fn load_frontiers(&self) -> HashMap<u64, WorkspaceSeenFrontier> {
        load_history_map("seen_frontiers")
    }
    fn save_frontiers(&self, frontiers: &HashMap<u64, WorkspaceSeenFrontier>) {
        save_history_map("seen_frontiers", frontiers);
    }
    fn load_departures(&self) -> HashMap<u64, WorkspaceDeparture> {
        load_history_map("departures")
    }
    fn save_departures(&self, departures: &HashMap<u64, WorkspaceDeparture>) {
        save_history_map("departures", departures);
    }
    fn mutations_available(&self) -> bool {
        true
    }
    fn all_workspaces_available(&self) -> bool {
        true
    }
    fn app_display_name(&self, app: &str) -> Option<String> {
        crate::shell::launcher::display_name_for_app_id_without_waiting(app)
    }
    fn agent_members(&self, workspace: &Workspace) -> Vec<(String, tilo_action_policy::KindSet)> {
        workspace.agent_members()
    }
    fn agent_is_live(&self, room: &str, agent: &str) -> bool {
        self.broker
            .as_ref()
            .is_some_and(|broker| broker.agent_is_live_task_capable(room, agent))
    }
    fn dispatch_task(&self, room: &str, task: &str) -> Result<(), String> {
        self.broker
            .as_ref()
            .ok_or_else(|| "agent task dispatch is unavailable".to_string())
            .and_then(|broker| broker.dispatch_task(room, task).map(|_| ()))
    }
    fn launch_agent(&self, agent: &str, workspace: &Workspace) -> Result<(), String> {
        crate::shell::launcher::launch_workspace_agent(agent, workspace)
            .map(|_| ())
            .map_err(|error| error.user_message())
    }
}

impl ActivityTree for WorkspaceTree {
    fn document(&self) -> &automerge::AutoCommit {
        &self.doc
    }
    fn document_mut(&mut self) -> &mut automerge::AutoCommit {
        &mut self.doc
    }
    fn commit_activity_change(&mut self) {
        self.commit_room_change();
    }
    /// Every room's agents whose delegation its governance verified for
    /// exactly their device ([`WorkspaceTree::verified_agents`]).
    fn roster_agents(&self) -> Vec<ui::history_source::RosterAgent> {
        let rooms = self
            .desktop
            .workspaces
            .iter()
            .map(|workspace| workspace.room)
            .collect::<BTreeSet<_>>();
        rooms
            .into_iter()
            .flat_map(|room| self.verified_agents(room))
            .collect()
    }
    fn resync_activity_cache(&mut self) -> Result<(), String> {
        self.resync_cache_from_doc();
        Ok(())
    }
    fn native_manual_tasks(&self, workspace: WorkspaceId) -> bool {
        self.room_of(workspace)
            .is_some_and(|room| self.requires_native_owner(room))
    }
    fn create_workspace_task(
        &mut self,
        workspace: WorkspaceId,
        title: String,
        actor: String,
        agent: Option<String>,
        route: WorkspaceTaskRoute,
        resource: Option<WorkspaceTaskResource>,
    ) -> Option<String> {
        WorkspaceTree::create_workspace_task(self, workspace, title, actor, agent, route, resource)
    }
    fn set_workspace_task_status(
        &mut self,
        workspace: WorkspaceId,
        id: &str,
        status: WorkspaceTaskStatus,
        actor: &str,
    ) -> bool {
        WorkspaceTree::set_workspace_task_status(self, workspace, id, status, actor)
    }
}

/// The shared pane with the native services installed.
pub struct RoomActivityPane(
    tilo_desktop::activity::RoomActivityPane,
    Option<crate::shell::collab_broker::AppActionBroker>,
    ShortcutRunRecorder,
    ColdRecordReads,
);

impl std::ops::Deref for RoomActivityPane {
    type Target = tilo_desktop::activity::RoomActivityPane;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for RoomActivityPane {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
impl Default for RoomActivityPane {
    fn default() -> Self {
        Self::new()
    }
}
impl Drop for RoomActivityPane {
    /// The session is ending: the departure it saved last is on disk before
    /// the compositor goes.
    fn drop(&mut self) {
        settle_history_writes();
    }
}
impl RoomActivityPane {
    /// Derive history and load referenced records after releasing the shared
    /// workspace lock. Admission and output may read the current tree while
    /// expensive history reconstruction uses this coherent observation.
    #[cfg(test)]
    pub(crate) fn refresh_observed(
        &mut self,
        tree: &WorkspaceTreeHandle,
        records: &RecordStore,
        local: &dyn tilo_desktop::activity::PersistedRecordHistory,
    ) -> u64 {
        refresh_pane(&mut self.0, tree, records, local)
    }

    /// Derive history from a coherent observation of `tree`, after the
    /// workspace lock is released, over the records `local` found on this
    /// device as a worker last read them: the refresh never reads the
    /// disk. A record not read yet is unavailable for now; what this
    /// refresh asked for is handed to the worker, and
    /// [`Self::take_persisted_reads`] says when Activity must refresh
    /// again because a read came in.
    pub(crate) fn refresh_observed_local(
        &mut self,
        tree: &WorkspaceTreeHandle,
        records: &RecordStore,
        local: &RoomRecords,
        now: Instant,
    ) -> u64 {
        let workspace = refresh_pane(&mut self.0, tree, records, &self.3);
        self.3.request(local.local_reader(), now);
        workspace
    }

    /// Take in the persisted records the worker read since the last call.
    /// Returns whether what Activity derives from changed, so it must
    /// refresh. Never waits.
    pub(crate) fn take_persisted_reads(&mut self) -> bool {
        self.3.take_reads()
    }

    #[cfg(test)]
    pub(crate) fn persisted_reads(&self) -> &ColdRecordReads {
        &self.3
    }

    pub fn new() -> Self {
        let mut pane = tilo_desktop::activity::RoomActivityPane::new();
        pane.set_host(Rc::new(NativeActivityHost::default()));
        Self(
            pane,
            None,
            ShortcutRunRecorder::default(),
            ColdRecordReads::default(),
        )
    }
    /// The production pane: the device's private seen frontiers and
    /// departures are read from and written to the user's state directory.
    pub fn new_persistent() -> Self {
        let mut pane = Self::new();
        pane.load_frontiers();
        pane
    }
    pub(crate) fn with_local_user(user: String) -> Self {
        let mut pane = tilo_desktop::activity::RoomActivityPane::with_local_user(user);
        pane.set_host(Rc::new(NativeActivityHost::default()));
        Self(
            pane,
            None,
            ShortcutRunRecorder::default(),
            ColdRecordReads::default(),
        )
    }
    /// Attach the live broker after the session binds. Without it, agent
    /// tasks remain visible but cannot be launched into an offline queue.
    pub fn attach_task_broker(&mut self, broker: crate::shell::collab_broker::AppActionBroker) {
        self.1 = Some(broker.clone());
        self.set_host(Rc::new(NativeActivityHost {
            morning: Some(Rc::new(super::morning_sources::NativeMorningSources::new(
                broker.clone(),
            ))),
            broker: Some(broker),
            review: None,
            undos: super::task_undo::TaskUndos::default(),
        }));
    }
    pub(crate) fn attach_task_review(&mut self, review: super::task_review::NativeTaskReview) {
        let broker = self.1.clone();
        self.set_host(Rc::new(NativeActivityHost {
            morning: broker
                .clone()
                .map(|broker| Rc::new(super::morning_sources::NativeMorningSources::new(broker))),
            broker,
            review: Some(review),
            undos: super::task_undo::TaskUndos::default(),
        }));
    }
    /// Whether a finished shortcut run waits to be recorded. Cheap: the
    /// frame pump asks every tick before taking any lock.
    pub(crate) fn shortcut_runs_pending(&self) -> bool {
        self.2.has_work(self.1.as_ref())
    }
    /// Record the shortcut runs the broker finished in `tree`'s document,
    /// each as one undoable run; see [`ShortcutRunRecorder::record`].
    /// `lock_tree` is called only to write a run that was found.
    pub(crate) fn record_finished_shortcut_runs<T: std::ops::DerefMut<Target = WorkspaceTree>>(
        &mut self,
        lock_tree: impl FnOnce() -> T,
        records: &RecordStore,
        local: &RoomRecords,
        now: Instant,
    ) -> bool {
        self.2
            .record(self.1.as_ref(), lock_tree, records, local, now)
    }
    #[cfg(test)]
    pub(crate) fn shortcut_run_recorder_mut(&mut self) -> &mut ShortcutRunRecorder {
        &mut self.2
    }
    /// Whether `agent` is admitted to `room` and holds a live task
    /// subscription right now. Without a broker no agent is ever present.
    pub fn agent_is_present_in(&self, room: &str, agent: &str) -> bool {
        self.1
            .as_ref()
            .is_some_and(|broker| broker.agent_is_live_task_capable(room, agent))
    }
    /// Dispatch a queued task in `room` to its assigned live agent, for
    /// surfaces outside Task Center that hand work over directly.
    ///
    /// # Errors
    ///
    /// Returns an error when no broker is attached or the assigned agent
    /// cannot accept the task.
    pub fn dispatch_task_in(&self, room: &str, task_id: &str) -> Result<u64, String> {
        self.1
            .as_ref()
            .ok_or_else(|| "agent task dispatch is unavailable".to_string())?
            .dispatch_task(room, task_id)
    }
    pub fn apply_deep_link(&mut self, focus: crate::shell::chrome_state::ActivityFocus) {
        self.0
            .apply_deep_link(tilo_desktop::activity::ActivityDeepLink {
                task_id: focus.task_id,
                author: focus.author,
                group_id: focus.group_id,
                entry_id: focus.entry_id,
                resource_id: focus.resource_id,
                resource_version: focus.resource_version,
            });
    }
    pub fn observe_agent_launch_event(
        &mut self,
        event: &crate::shell::launcher::LaunchEvent,
    ) -> bool {
        self.0
            .observe_agent_launch_event(&activity_launch_event(event))
    }
}

/// Refresh `pane` from a coherent observation of `tree`, deriving history
/// after the workspace lock is released, with persisted records from
/// `local`.
fn refresh_pane(
    pane: &mut tilo_desktop::activity::RoomActivityPane,
    tree: &WorkspaceTreeHandle,
    records: &RecordStore,
    local: &dyn tilo_desktop::activity::PersistedRecordHistory,
) -> u64 {
    let mut observed = ActivityObservation::capture(tree);
    let workspace = observed.active.raw();
    pane.observe_workspace(workspace);
    pane.refresh(&mut observed);
    pane.refresh_records_with_local(&mut observed, records, local);
    workspace
}

fn activity_launch_event(
    event: &crate::shell::launcher::LaunchEvent,
) -> tilo_desktop::activity::AgentLaunchEvent {
    use crate::shell::launcher::LaunchEventKind as Native;
    use tilo_desktop::activity::AgentLaunchEventKind as Shared;
    tilo_desktop::activity::AgentLaunchEvent {
        managed_agent_id: event.managed_agent_id.clone(),
        managed_workspace_id: event.managed_workspace_id.clone(),
        kind: match &event.kind {
            Native::Started { pid } => Shared::Started { pid: *pid },
            Native::SpawnFailed { detail, .. } => Shared::SpawnFailed {
                detail: detail.clone(),
            },
            Native::Failed { code, stderr_tail } | Native::WindowLost { code, stderr_tail } => {
                Shared::Failed {
                    code: *code,
                    stderr_tail: stderr_tail.clone(),
                }
            }
            Native::Exited => Shared::Exited,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::ReadDoc as _;
    use automerge::transaction::{CommitOptions, Transactable};
    use ui::record::{RecordId, RecordStore};

    #[test]
    fn history_refresh_releases_workspace_authority_before_loading_records() {
        struct LocalHistory {
            tree: WorkspaceTreeHandle,
            record: RecordId,
            document: automerge::AutoCommit,
            loads: std::cell::Cell<usize>,
        }
        impl tilo_desktop::activity::PersistedRecordHistory for LocalHistory {
            fn load_record(&self, id: &RecordId) -> Option<automerge::AutoCommit> {
                assert_eq!(id, &self.record);
                // The production refresh calls this after taking its coherent
                // observation. Neither a writer nor an admission reader must
                // wait for history I/O and reconstruction to finish.
                let mut live = self.tree.try_write().expect("history holds no tree lock");
                if self.loads.get() == 0 {
                    live.doc
                        .put(automerge::ROOT, "during-history", true)
                        .unwrap();
                    live.commit_room_change();
                }
                drop(live);
                self.loads.set(self.loads.get() + 1);
                Some(self.document.clone())
            }
        }

        let mut tree = WorkspaceTree::bootstrap_demo();
        let room = tree.active_workspace().expect("workspace").room;
        let record = RecordId::for_file("history-lock.qdoc");
        let kind = ui::RecordKind::new("document");
        tree.room_records_held
            .entry(room)
            .or_default()
            .insert(record.to_string(), kind.to_string());
        tree.commit_room_change();
        let handle = WorkspaceTreeHandle::new(tree);
        let mut document = automerge::AutoCommit::new();
        ui::record::init(&mut document, &record, &kind, &room.to_string());
        document
            .put(automerge::ROOT, "draft", "History remains available")
            .unwrap();
        tilo_resource::history::commit_stamped_at(&mut document, 21);
        let local = LocalHistory {
            tree: handle.clone(),
            record,
            document,
            loads: std::cell::Cell::new(0),
        };
        let mut pane = RoomActivityPane::with_local_user("local".into());
        let records = RecordStore::in_memory();
        pane.refresh_observed(&handle, &records, &local);
        assert!(
            local.loads.get() > 0,
            "the production path loaded record history"
        );
        let heads = handle.read().doc.base_heads();
        assert!(
            handle
                .read()
                .doc
                .get(automerge::ROOT, "during-history")
                .unwrap()
                .is_some()
        );
        assert!(!pane.entries().is_empty(), "history is still projected");
        pane.refresh_observed(&handle, &records, &local);
        drop(pane);
        assert_eq!(
            handle.read().doc.base_heads(),
            heads,
            "derived cursors never replace live state"
        );
    }

    /// Activity's history reads a record persisted on this device (not
    /// open here) off the loop: while the read is held up, as by a disk
    /// under pressure, every refresh returns with the record unavailable;
    /// once the read is in, the next refresh shows its history. A peer's
    /// edit that moves the file on disk is read the same way: the refresh
    /// keeps the last-read history until the new read is in.
    #[test]
    fn activity_never_reads_a_persisted_record_on_the_loop() {
        let mut tree = WorkspaceTree::bootstrap_demo();
        let room = tree.active_workspace().expect("workspace").room;
        let record = RecordId::for_file("activity-cold-read.qdoc");
        let kind = ui::RecordKind::new("document");
        tree.room_records_held
            .entry(room)
            .or_default()
            .insert(record.to_string(), kind.to_string());
        tree.commit_room_change();
        let handle = WorkspaceTreeHandle::new(tree);

        let data = tempfile::tempdir().expect("data home");
        let path = RoomRecords::record_document_path_for_test(
            data.path(),
            "so.tilo.fixture-drafts",
            "activity-cold-read",
        );
        std::fs::create_dir_all(path.parent().expect("record directory")).unwrap();
        let mut document = automerge::AutoCommit::new();
        ui::record::init(&mut document, &record, &kind, &room.to_string());
        document
            .put(automerge::ROOT, "draft", "Written before the refresh")
            .unwrap();
        tilo_resource::history::commit_stamped_at(&mut document, 21);
        std::fs::write(&path, document.save()).unwrap();
        let mut local = RoomRecords::in_memory();
        local.scan_now_for_test(data.path());

        let records = RecordStore::in_memory();
        let mut pane = RoomActivityPane::with_local_user("local".into());
        let interval = std::time::Duration::from_secs(1);
        let mut at = Instant::now();
        let release = crate::shell::cold_record_reads::held::hold_record(record.clone());
        for _ in 0..3 {
            pane.refresh_observed_local(&handle, &records, &local, at);
            assert!(!pane.take_persisted_reads(), "the read is held up");
            assert!(pane.persisted_reads().reading(), "the read is off the loop");
            assert!(
                pane.feed_coverage()
                    .unavailable_records
                    .iter()
                    .any(|checkout| checkout.id == record),
                "a record not read yet is unavailable, not read here"
            );
            at = at.checked_add(interval).expect("a later frame");
        }
        let before = pane.entries().len();

        release.send(()).expect("the read is held");
        wait_for_reads(&mut pane);
        pane.refresh_observed_local(&handle, &records, &local, at);
        assert!(pane.feed_coverage().is_complete(), "the read is in");
        assert_eq!(pane.feed_coverage().indexed_records, 1);
        let read = pane.entries().len();
        assert!(read > before, "the record's history is shown");

        // That refresh handed the record to the worker again; nothing
        // moved, so the pass changes nothing.
        let deadline = Instant::now()
            .checked_add(std::time::Duration::from_secs(30))
            .expect("a deadline");
        while pane.persisted_reads().reading() {
            assert!(
                !pane.take_persisted_reads(),
                "an unchanged file is not read"
            );
            assert!(Instant::now() < deadline, "the pass never finished");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }

        // A peer's edit reaches the file. The next refresh still derives
        // from the last read and hands the record to the worker, which
        // reads the moved file; the refresh after shows the edit.
        document
            .put(automerge::ROOT, "draft", "Edited by a peer")
            .unwrap();
        tilo_resource::history::commit_stamped_at(&mut document, 42);
        std::fs::write(&path, document.save()).unwrap();
        at = at.checked_add(interval).expect("a later frame");
        let release = crate::shell::cold_record_reads::held::hold_record(record.clone());
        pane.refresh_observed_local(&handle, &records, &local, at);
        assert_eq!(pane.entries().len(), read, "the last read stands meanwhile");
        assert!(pane.feed_coverage().is_complete());
        assert!(
            pane.persisted_reads().reading(),
            "the moved file is read off the loop"
        );
        assert!(!pane.take_persisted_reads(), "the read is held up");
        release.send(()).expect("the read is held");
        wait_for_reads(&mut pane);
        at = at.checked_add(interval).expect("a later frame");
        pane.refresh_observed_local(&handle, &records, &local, at);
        let edited = pane.entries().len();
        drop(pane);
        assert!(edited > read, "the peer's edit is shown");
    }

    /// Take the persisted reads in once the worker has them. Polls: tests
    /// only.
    fn wait_for_reads(pane: &mut RoomActivityPane) {
        let deadline = Instant::now()
            .checked_add(std::time::Duration::from_secs(30))
            .expect("a deadline");
        while !pane.take_persisted_reads() {
            assert!(Instant::now() < deadline, "the read never came in");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    /// Leaving a workspace rewrites the history file and nothing else: a
    /// write to `settings.json` would make every settings reader re-apply
    /// its preferences on each workspace switch.
    #[test]
    fn morning_sources_private_persistence_preserves_review_boundaries_and_room_identity() {
        use std::os::unix::fs::PermissionsExt;
        use tilo_desktop::activity::morning::{MorningPreferences, MorningSource};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("activity-history.json");
        let frontier = HashMap::from([(4, WorkspaceSeenFrontier::default())]);
        save_history_map_at(&path, "seen_frontiers", &frontier).unwrap();
        let room = uuid::Uuid::new_v4().to_string();
        let source = MorningSource {
            app: "so.tilo.weather".into(),
            resource: "weather.location:fixture".into(),
        };
        let preferences = HashMap::from([(
            4,
            MorningPreferences {
                room: room.clone(),
                sources: vec![source.clone()],
            },
        )]);
        save_history_map_at(&path, "morning_sources", &preferences).unwrap();
        let reloaded: HashMap<u64, MorningPreferences> =
            load_history_map_at(Some(&path), "morning_sources");
        assert_eq!(reloaded[&4].for_room(&room), &[source]);
        assert!(
            reloaded[&4]
                .for_room(&uuid::Uuid::new_v4().to_string())
                .is_empty()
        );
        let observed: HashMap<u64, WorkspaceSeenFrontier> =
            load_history_map_at(Some(&path), "seen_frontiers");
        assert_eq!(observed, frontier);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    /// The palette lists the morning-source actions on every query, often
    /// right after leaving a workspace handed its frontiers and departures to
    /// the history writer. The query is served what this process saved and
    /// what the file held, and never waits for the writer's synced write.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_palette_after_a_workspace_switch_never_waits_for_the_history_writer() {
        use tilo_desktop::activity::morning::{MorningPreferences, MorningSource};
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("state/tilo/activity-history.json");
        let tree = WorkspaceTree::bootstrap_demo();
        let workspace = tree.desktop.active_workspace().expect("workspace").clone();
        let preferences = HashMap::from([(
            workspace.id.raw(),
            MorningPreferences {
                room: workspace.room.to_string(),
                sources: vec![MorningSource {
                    app: "so.tilo.weather".into(),
                    resource: "weather.location:fixture".into(),
                }],
            },
        )]);
        save_history_map_at(&path, "morning_sources", &preferences).expect("saved");
        // Started here, the writer does not inherit the guard's filter.
        let _ = history_writer();
        let (actions, departures, settles) =
            crate::shell::disk_sync_guard::on_a_thread_that_cannot_sync(|| {
                // Leaving a workspace hands both maps to the writer.
                queue_history_save(
                    path.clone(),
                    "seen_frontiers",
                    history_map_value(&HashMap::from([(9_u64, WorkspaceSeenFrontier::default())])),
                );
                queue_history_save(
                    path.clone(),
                    "departures",
                    history_map_value(&HashMap::from([(
                        7_u64,
                        WorkspaceDeparture {
                            departed_at_secs: 1_700_000_000,
                            frontier: WorkspaceSeenFrontier::default(),
                        },
                    )])),
                );
                // The palette's next query.
                let choices = load_known_history_map(&path, "morning_sources");
                let actions = morning_source_actions_from(&workspace, &choices);
                let departures: HashMap<u64, WorkspaceDeparture> =
                    load_known_history_map(&path, "departures");
                (
                    actions,
                    departures,
                    SETTLES_ON_THIS_THREAD.with(std::cell::Cell::get),
                )
            });
        assert_eq!(settles, 0, "a palette query waited for the history writer");
        assert_eq!(
            actions.len(),
            5,
            "the morning source the file held is offered"
        );
        assert_eq!(
            departures
                .get(&7)
                .map(|departure| departure.departed_at_secs),
            Some(1_700_000_000),
            "a save still on its way to disk is what a load returns"
        );
        settle_history_writes();
        let on_disk: HashMap<u64, WorkspaceDeparture> =
            load_history_map_at(Some(&path), "departures");
        assert!(on_disk.contains_key(&7), "the save still reaches the disk");
        let kept: HashMap<u64, MorningPreferences> =
            load_history_map_at(Some(&path), "morning_sources");
        assert_eq!(kept, preferences, "the writer keeps the file's other maps");
    }

    #[test]
    fn workspace_history_keeps_both_maps_in_its_own_private_file() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("state/tilo/activity-history.json");
        let departures = HashMap::from([(
            7_u64,
            WorkspaceDeparture {
                departed_at_secs: 1_700_000_000,
                frontier: WorkspaceSeenFrontier::default(),
            },
        )]);
        let frontiers = HashMap::from([(9_u64, WorkspaceSeenFrontier::default())]);
        save_history_map_at(&path, "departures", &departures).expect("departures saved");
        save_history_map_at(&path, "seen_frontiers", &frontiers).expect("frontiers saved");

        let loaded: HashMap<u64, WorkspaceDeparture> =
            load_history_map_at(Some(&path), "departures");
        assert_eq!(
            loaded.get(&7).map(|departure| departure.departed_at_secs),
            Some(1_700_000_000),
            "saving the frontiers keeps the departures beside them"
        );
        let loaded: HashMap<u64, WorkspaceSeenFrontier> =
            load_history_map_at(Some(&path), "seen_frontiers");
        assert!(loaded.contains_key(&9));
        let mode = std::fs::metadata(&path)
            .expect("saved")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        assert!(
            history_path().is_none_or(|history| Some(history) != settings_store::settings_path()),
            "history must not live in settings.json"
        );
        let missing: HashMap<u64, WorkspaceDeparture> =
            load_history_map_at(Some(&dir.path().join("absent.json")), "departures");
        assert!(missing.is_empty());
    }

    /// Leaving a workspace hands the history to the writer thread instead
    /// of syncing the file inside the frame: saves queue without touching
    /// the disk, the newest map per key wins, and a settled writer has both
    /// maps in one private file.
    #[test]
    fn queued_history_saves_reach_disk_with_the_newest_map_per_key() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("state/tilo/activity-history.json");
        let departure = |secs| {
            HashMap::from([(
                7_u64,
                WorkspaceDeparture {
                    departed_at_secs: secs,
                    frontier: WorkspaceSeenFrontier::default(),
                },
            )])
        };
        queue_history_save(
            path.clone(),
            "departures",
            history_map_value(&departure(1_700_000_000)),
        );
        queue_history_save(
            path.clone(),
            "seen_frontiers",
            history_map_value(&HashMap::from([(9_u64, WorkspaceSeenFrontier::default())])),
        );
        queue_history_save(
            path.clone(),
            "departures",
            history_map_value(&departure(1_700_000_060)),
        );
        settle_history_writes();

        let loaded: HashMap<u64, WorkspaceDeparture> =
            load_history_map_at(Some(&path), "departures");
        assert_eq!(
            loaded.get(&7).map(|departure| departure.departed_at_secs),
            Some(1_700_000_060),
            "the newest departure wins"
        );
        let loaded: HashMap<u64, WorkspaceSeenFrontier> =
            load_history_map_at(Some(&path), "seen_frontiers");
        assert!(loaded.contains_key(&9), "both maps share the file");
        let mode = std::fs::metadata(&path)
            .expect("saved")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    /// A shortcut run the broker finished lands in the workspace document
    /// as one undoable run as soon as a document it changed is open here.
    #[test]
    fn a_finished_shortcut_run_is_recorded_once_from_the_documents_it_changed() {
        let mut tree = WorkspaceTree::bootstrap_demo();
        let room = tree.desktop.active_workspace().expect("workspace").room;
        let records = RecordStore::in_memory();
        let record = RecordId::for_file("drafts.qdoc");
        let checkout = ui::RecordCheckout::new(record.clone(), ui::RecordKind::new("document"));
        assert!(records.open_record(&checkout));
        let run = tilo_automation::RunId::new();
        records.with(&record, |doc| {
            ui::record::init(doc, &record, &checkout.kind, &room.to_string());
            doc.put(automerge::ROOT, "draft", "Launch notes").unwrap();
            let context = ui::tilo_actions::ActionContext::for_resource(
                "fixture.drafts:inbox",
                "fixture.drafts",
                "Inbox",
            )
            .with_app("so.tilo.fixture-drafts", "Drafts")
            .with_group(run.0.to_string(), "Publish a draft", "shortcut");
            doc.commit_with(
                CommitOptions::default().with_message(context.change_message().unwrap()),
            );
        });
        let shortcut: tilo_automation::Shortcut = serde_json::from_value(
            serde_json::json!({"id": "publish", "name": "Publish a draft", "steps": []}),
        )
        .unwrap();
        let finished = PendingShortcutRun::new(
            crate::shell::automation_host::FinishedShortcutRun {
                room: room.to_string(),
                shortcut,
                invoker: "ada".into(),
                report: tilo_automation::RunReport {
                    run,
                    shortcut: "publish".into(),
                    trigger: "manual".into(),
                    mode: tilo_automation::RunMode::Unattended,
                    steps: Vec::new(),
                    output: None,
                    outcome: tilo_automation::RunOutcome::Completed,
                },
            },
            Instant::now(),
        );
        let mut pane = RoomActivityPane::new();
        pane.shortcut_run_recorder_mut().push(finished);
        let local = RoomRecords::in_memory();

        let now = Instant::now();
        let next = now
            .checked_add(SHORTCUT_RUN_LOOK_INTERVAL)
            .expect("the next look");
        pane.record_finished_shortcut_runs(locked(&mut tree), &records, &local, now);
        pane.record_finished_shortcut_runs(locked(&mut tree), &records, &local, next);

        let runs = ui::read_runs(&tree.doc);
        assert_eq!(runs.len(), 1, "recorded exactly once");
        assert_eq!(runs[0].label, "Publish a draft");
        assert_eq!(runs[0].issued_by.as_deref(), Some("ada"));
        assert_eq!(runs[0].steps[0].record, record.to_string());
        assert_eq!(runs[0].steps[0].app, "so.tilo.fixture-drafts");
        let pending_left = pane.shortcut_run_recorder_mut().pending_len();
        drop(pane);
        assert_eq!(pending_left, 0, "the recorded run is no longer pending");
    }

    /// An agent's run of a workspace task that ended is recorded as one run
    /// from the changes it grouped under the task: under the task's title,
    /// by the agent, for the person, and naming the task, which is what the
    /// Task Center offers to undo and what undo names. Changes of another
    /// task are not its.
    #[test]
    fn an_ended_task_run_is_recorded_as_the_agents_undoable_run() {
        let mut tree = WorkspaceTree::bootstrap_demo();
        let room = tree.desktop.active_workspace().expect("workspace").room;
        let records = RecordStore::in_memory();
        let record = RecordId::for_file("launch.note");
        let checkout = ui::RecordCheckout::new(record.clone(), ui::RecordKind::new("note"));
        assert!(records.open_record(&checkout));
        records.with(&record, |doc| {
            ui::record::init(doc, &record, &checkout.kind, &room.to_string());
            for (key, task) in [("body", "task:translate"), ("title", "task:other")] {
                doc.put(automerge::ROOT, key, "Hallo").unwrap();
                let context = ui::tilo_actions::ActionContext::for_resource(
                    "notes.note:launch",
                    "notes.note",
                    "Launch notes",
                )
                .with_app("so.tilo.notes", "Notes")
                .with_group(task, "Translate", "task");
                doc.commit_with(
                    CommitOptions::default().with_message(context.change_message().unwrap()),
                );
            }
        });
        let mut pane = RoomActivityPane::new();
        pane.shortcut_run_recorder_mut()
            .push(PendingShortcutRun::ended(
                EndedRun::Task(EndedTaskRun {
                    room: room.to_string(),
                    task_id: "task:translate".into(),
                    title: "Translate the paragraph".into(),
                    agent: "agent:scribe".into(),
                    person: "person:ada".into(),
                    instruction: Some("translate this to German in place".into()),
                    outcome: "completed".into(),
                }),
                Instant::now(),
            ));
        let local = RoomRecords::in_memory();
        let now = Instant::now();
        assert!(pane.record_finished_shortcut_runs(locked(&mut tree), &records, &local, now));
        drop(pane);

        let runs = ui::read_runs(&tree.doc);
        assert_eq!(runs.len(), 1);
        let run = &runs[0];
        assert_eq!(run.label, "Translate the paragraph");
        assert_eq!(run.by.as_deref(), Some("agent:scribe"));
        assert_eq!(run.issued_by.as_deref(), Some("person:ada"));
        assert_eq!(
            run.instruction.as_deref(),
            Some("translate this to German in place")
        );
        assert_eq!(run.task.as_deref(), Some("task:translate"));
        assert_eq!(run.steps.len(), 1, "only the task's own change");
        assert_eq!(run.steps[0].app, "so.tilo.notes");
        assert_eq!(run.steps[0].record, record.to_string());
        assert_eq!(run.steps[0].run.start, run.steps[0].run.end);
    }

    /// An app whose content is the room's own document (Notes keeps its
    /// notebook there) keeps it beside its records, and nothing announces
    /// it as one. An agent's task run that changed it is still recorded,
    /// from that document as this device holds it.
    #[test]
    fn a_task_run_over_an_apps_room_document_is_recorded() {
        let mut tree = WorkspaceTree::bootstrap_demo();
        let room = tree.desktop.active_workspace().expect("workspace").room;
        let data = tempfile::tempdir().expect("data home");
        let path = RoomRecords::room_document_path_for_test(data.path(), "so.tilo.notes", room);
        std::fs::create_dir_all(path.parent().expect("room directory")).unwrap();
        let mut doc = automerge::AutoCommit::new();
        doc.put(automerge::ROOT, "body", "Hello").unwrap();
        doc.commit();
        doc.put(automerge::ROOT, "body", "Hallo").unwrap();
        let context = ui::tilo_actions::ActionContext::for_resource(
            "notes.notebook/pages/one",
            "notes.page",
            "Launch notes",
        )
        .with_app("so.tilo.notes", "Notes")
        .with_group("task:translate", "Translate", "task");
        doc.commit_with(CommitOptions::default().with_message(context.change_message().unwrap()));
        std::fs::write(&path, doc.save()).unwrap();
        let mut local = RoomRecords::in_memory();
        local.scan_now_for_test(data.path());
        let records = RecordStore::in_memory();

        let mut pane = RoomActivityPane::new();
        pane.shortcut_run_recorder_mut()
            .push(PendingShortcutRun::ended(
                EndedRun::Task(EndedTaskRun {
                    room: room.to_string(),
                    task_id: "task:translate".into(),
                    title: "Translate".into(),
                    agent: "agent:scribe".into(),
                    person: "person:ada".into(),
                    instruction: None,
                    outcome: "completed".into(),
                }),
                Instant::now(),
            ));
        let now = Instant::now();
        assert!(!pane.record_finished_shortcut_runs(locked(&mut tree), &records, &local, now));
        pane.shortcut_run_recorder_mut().finish_reading();
        assert!(pane.record_finished_shortcut_runs(locked(&mut tree), &records, &local, now));
        drop(pane);

        let runs = ui::read_runs(&tree.doc);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].task.as_deref(), Some("task:translate"));
        assert_eq!(runs[0].steps.len(), 1);
        assert_eq!(runs[0].steps[0].app, "so.tilo.notes");
        assert_eq!(
            Some(runs[0].steps[0].record.clone()),
            crate::shell::room_records::room_document_id(room, "so.tilo.notes")
                .map(|id| id.to_string())
        );
    }

    /// `tree` as the recorder locks it.
    fn locked<'a>(tree: &'a mut WorkspaceTree) -> impl FnOnce() -> &'a mut WorkspaceTree + 'a {
        move || tree
    }

    /// A change the "Publish a draft" fixture app makes as a step of `run`.
    fn commit_draft_step(doc: &mut automerge::AutoCommit, run: tilo_automation::RunId) {
        doc.put(automerge::ROOT, "draft", "Launch notes").unwrap();
        let context = ui::tilo_actions::ActionContext::for_resource(
            "fixture.drafts:inbox",
            "fixture.drafts",
            "Inbox",
        )
        .with_app("so.tilo.fixture-drafts", "Drafts")
        .with_described(ui::tilo_actions::Described::new(
            ui::tilo_actions::Did::verb(
                ui::tilo_actions::ActionKey::new(
                    "so.tilo.fixture-drafts",
                    "fixture.drafts",
                    "write-draft",
                ),
                [],
            ),
        ))
        .with_group(run.0.to_string(), "Publish a draft", "shortcut");
        doc.commit_with(CommitOptions::default().with_message(context.change_message().unwrap()));
    }

    /// The broker's report of "Publish a draft" run `run` in `room`, by Ada,
    /// whose one step invoked the fixture app.
    fn finished_invoked_run(
        room: crate::shell::state::RoomId,
        run: tilo_automation::RunId,
    ) -> FinishedShortcutRun {
        let shortcut: tilo_automation::Shortcut = serde_json::from_value(
            serde_json::json!({"id": "publish", "name": "Publish a draft", "steps": []}),
        )
        .unwrap();
        let path = tilo_automation::StepPath::root().child(0);
        FinishedShortcutRun {
            room: room.to_string(),
            shortcut,
            invoker: "ada".into(),
            report: tilo_automation::RunReport {
                run,
                shortcut: "publish".into(),
                trigger: "manual".into(),
                mode: tilo_automation::RunMode::Unattended,
                steps: vec![tilo_automation::StepRecord {
                    invocation: Some(tilo_automation::invocation_id(run, &path)),
                    path,
                    step: "draft".into(),
                    status: tilo_automation::StepStatus::Succeeded { output: None },
                }],
                output: None,
                outcome: tilo_automation::RunOutcome::Completed,
            },
        }
    }

    /// A shortcut whose step changed a record that is not open on this
    /// desktop (an app opened it headless, edited and saved it) is still
    /// recorded as one undoable run, read from the record on disk once the
    /// change has been written there.
    #[test]
    fn a_shortcut_run_over_a_closed_record_is_still_recorded() {
        let mut tree = WorkspaceTree::bootstrap_demo();
        let room = tree.desktop.active_workspace().expect("workspace").room;
        let records = RecordStore::in_memory();
        let record = RecordId::for_file("receipts.sheet");
        let kind = ui::RecordKind::new("sheet");
        let run = tilo_automation::RunId::new();

        // The record exists on disk before the run, in the app's own store.
        let data = tempfile::tempdir().expect("data home");
        let path = RoomRecords::record_document_path_for_test(
            data.path(),
            "so.tilo.fixture-drafts",
            "receipts",
        );
        std::fs::create_dir_all(path.parent().expect("record directory")).unwrap();
        let mut doc = automerge::AutoCommit::new();
        ui::record::init(&mut doc, &record, &kind, &room.to_string());
        doc.commit();
        std::fs::write(&path, doc.save()).unwrap();
        let mut local = RoomRecords::in_memory();
        local.scan_now_for_test(data.path());
        assert!(
            records.open_records().is_empty(),
            "the record is not open on this desktop"
        );

        let mut pane = RoomActivityPane::new();
        let finished_at = Instant::now();
        pane.shortcut_run_recorder_mut()
            .push(PendingShortcutRun::new(
                finished_invoked_run(room, run),
                finished_at,
            ));

        // The run finished before the app's save reached the disk: nothing
        // to record yet, and the run keeps waiting. The look reads the
        // record off the loop; the run is judged once that read is in.
        assert!(!pane.record_finished_shortcut_runs(
            locked(&mut tree),
            &records,
            &local,
            finished_at
        ));
        pane.shortcut_run_recorder_mut().finish_reading();
        assert!(!pane.record_finished_shortcut_runs(
            locked(&mut tree),
            &records,
            &local,
            finished_at
        ));
        assert!(ui::read_runs(&tree.doc).is_empty());
        assert!(
            pane.shortcut_runs_pending(),
            "the run waits for its changes"
        );

        // The app's save lands; the next look reads the changed record.
        commit_draft_step(&mut doc, run);
        std::fs::write(&path, doc.save()).unwrap();
        let next = finished_at
            .checked_add(SHORTCUT_RUN_LOOK_INTERVAL)
            .expect("the next look");
        assert!(!pane.record_finished_shortcut_runs(locked(&mut tree), &records, &local, next));
        pane.shortcut_run_recorder_mut().finish_reading();
        assert!(pane.record_finished_shortcut_runs(locked(&mut tree), &records, &local, next));
        let still_pending = pane.shortcut_runs_pending();
        drop(pane);
        assert!(!still_pending, "the recorded run no longer waits");

        let runs = ui::read_runs(&tree.doc);
        assert_eq!(runs.len(), 1, "the run is recorded once");
        assert_eq!(runs[0].label, "Publish a draft");
        assert_eq!(runs[0].issued_by.as_deref(), Some("ada"));
        assert_eq!(runs[0].steps.len(), 1, "one step, for the closed record");
        assert_eq!(runs[0].steps[0].record, record.to_string());
        assert_eq!(runs[0].steps[0].app, "so.tilo.fixture-drafts");
        assert_eq!(runs[0].steps[0].sentence(), "Write draft");
        assert_eq!(
            runs[0].steps[0].run.start,
            ui::history_source::entry_id_from_hash(
                &doc.get_heads()
                    .first()
                    .expect("the step's change")
                    .to_string()
            ),
            "the step names the exact change the run made"
        );
    }

    /// Looking for a finished run's changes in a record on disk reads the
    /// record off the loop and never under the workspace tree's lock: while
    /// the read is held up, as by a disk under pressure, every pass returns
    /// at once without locking the tree, and the run is recorded on the
    /// first pass after the read is in.
    #[test]
    fn a_shortcut_run_look_never_reads_the_disk_on_the_loop_or_under_the_tree_lock() {
        let mut tree = WorkspaceTree::bootstrap_demo();
        let room = tree.desktop.active_workspace().expect("workspace").room;
        let records = RecordStore::in_memory();
        let record = RecordId::for_file("receipts.sheet");
        let kind = ui::RecordKind::new("sheet");
        let run = tilo_automation::RunId::new();
        let data = tempfile::tempdir().expect("data home");
        let path = RoomRecords::record_document_path_for_test(
            data.path(),
            "so.tilo.fixture-drafts",
            "receipts",
        );
        std::fs::create_dir_all(path.parent().expect("record directory")).unwrap();
        let mut doc = automerge::AutoCommit::new();
        ui::record::init(&mut doc, &record, &kind, &room.to_string());
        doc.commit();
        commit_draft_step(&mut doc, run);
        std::fs::write(&path, doc.save()).unwrap();
        let mut local = RoomRecords::in_memory();
        local.scan_now_for_test(data.path());

        let mut pane = RoomActivityPane::new();
        let finished_at = Instant::now();
        pane.shortcut_run_recorder_mut()
            .push(PendingShortcutRun::new(
                finished_invoked_run(room, run),
                finished_at,
            ));
        let release = held_looks::hold_run(run);
        let untouched = || -> &'static mut WorkspaceTree {
            panic!("the tree was locked while the record was being read")
        };
        let mut at = finished_at;
        for _ in 0..3 {
            assert!(!pane.record_finished_shortcut_runs(untouched, &records, &local, at));
            assert!(
                pane.shortcut_run_recorder_mut().reading(),
                "the read is still held up"
            );
            at = at
                .checked_add(SHORTCUT_RUN_LOOK_INTERVAL)
                .expect("a later pass");
        }
        assert!(ui::read_runs(&tree.doc).is_empty());

        release.send(()).expect("the read is held");
        pane.shortcut_run_recorder_mut().finish_reading();
        assert!(pane.record_finished_shortcut_runs(locked(&mut tree), &records, &local, at));
        drop(pane);
        let runs = ui::read_runs(&tree.doc);
        assert_eq!(runs.len(), 1, "recorded once the read was in");
        assert_eq!(runs[0].steps[0].record, record.to_string());
    }

    /// A run whose changes never reach a document here waits on the wall
    /// clock, not on how many frames were painted, and is then given up.
    #[test]
    fn a_run_that_changed_nothing_here_is_given_up_after_its_settle_time() {
        let mut tree = WorkspaceTree::bootstrap_demo();
        let room = tree.desktop.active_workspace().expect("workspace").room;
        let records = RecordStore::in_memory();
        let local = RoomRecords::in_memory();
        let mut pane = RoomActivityPane::new();
        let finished_at = Instant::now();
        pane.shortcut_run_recorder_mut()
            .push(PendingShortcutRun::new(
                finished_invoked_run(room, tilo_automation::RunId::new()),
                finished_at,
            ));

        // However many looks happen inside the settle time, the run waits.
        let settled = finished_at
            .checked_add(SHORTCUT_RUN_SETTLE)
            .expect("the settle time");
        let mut at = finished_at;
        while at < settled {
            assert!(!pane.record_finished_shortcut_runs(locked(&mut tree), &records, &local, at));
            assert!(pane.shortcut_runs_pending());
            at = at
                .checked_add(SHORTCUT_RUN_LOOK_INTERVAL)
                .expect("the next look");
        }
        assert!(!pane.record_finished_shortcut_runs(locked(&mut tree), &records, &local, at));
        let still_pending = pane.shortcut_runs_pending();
        drop(pane);
        assert!(!still_pending, "given up once settled");
        assert!(ui::read_runs(&tree.doc).is_empty());
    }
}

#[cfg(test)]
mod morning_host_regression {
    use super::*;
    #[test]
    fn native_morning_sources_advertise_private_selection() {
        assert!(NativeActivityHost::default().morning_sources_available());
    }
}
