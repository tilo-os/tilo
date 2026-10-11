//! Native services for the shared workspace activity pane.
//!
//! The pane itself is [`tilo_desktop::activity::RoomActivityPane`]; this
//! module gives it the device's identity, its private seen frontiers and
//! departures (kept in the user's state directory), live agent dispatch through the session's
//! action broker, managed agent launches, and the native workspace tree as
//! its causal history source.
use crate::shell::automation_host::{
    DocumentShare, FinishedShortcutRun, document_share, ordered_steps, record_run_steps,
};
use crate::shell::collab_broker::AppActionBroker;
use crate::shell::room_records::RoomRecords;
use crate::shell::state::WorkspaceAuthority as _;
use crate::shell::state::{
    Workspace, WorkspaceId, WorkspaceTaskResource, WorkspaceTaskRoute, WorkspaceTaskStatus,
    WorkspaceTree, WorkspaceTreeHandle,
};
use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;
use std::sync::{OnceLock, mpsc};
use std::time::{Duration, Instant};
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
        }
    }
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
}

/// A document's revision when a waiting run last looked at it: the heads of
/// a document open here, or the persisted witness of one on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
enum LookedRevision {
    Open(Vec<automerge::ChangeHash>),
    Persisted(u64),
}

/// A shortcut run the broker finished whose history is not recorded yet.
pub(crate) struct PendingShortcutRun {
    finished: FinishedShortcutRun,
    since: Instant,
    /// What the last look found in each candidate document, under the
    /// revision it was read at. `None` when the revision is unknown, so the
    /// document is read again on the next look.
    looked: HashMap<RecordId, (Option<LookedRevision>, Option<DocumentShare>)>,
}

impl PendingShortcutRun {
    pub(crate) fn new(finished: FinishedShortcutRun, since: Instant) -> Self {
        Self {
            finished,
            since,
            looked: HashMap::new(),
        }
    }

    /// The run's steps among `candidates`: documents open in `records` are
    /// read in memory, the others from `local`, and only those whose
    /// revision moved since the last look.
    fn look(
        &mut self,
        candidates: &BTreeSet<RecordId>,
        open: &BTreeSet<RecordId>,
        records: &RecordStore,
        local: &RoomRecords,
    ) -> Vec<ui::RunStep> {
        use automerge::transaction::Transactable as _;
        let run = self.finished.report.run;
        self.looked.retain(|id, _| candidates.contains(id));
        for id in candidates {
            let is_open = open.contains(id);
            let revision = if is_open {
                records.read(id, |doc| LookedRevision::Open(doc.base_heads()))
            } else {
                local
                    .local_record_revision(id)
                    .map(LookedRevision::Persisted)
            };
            if revision.is_some()
                && self
                    .looked
                    .get(id)
                    .is_some_and(|(seen, _)| *seen == revision)
            {
                continue;
            }
            let share = if is_open {
                records
                    .read(id, |doc| document_share(run, id.as_str(), doc))
                    .flatten()
            } else {
                local
                    .load_local_record(id)
                    .and_then(|doc| document_share(run, id.as_str(), &doc))
            };
            self.looked.insert(id.clone(), (revision, share));
        }
        ordered_steps(
            self.looked
                .values()
                .filter_map(|(_, share)| share.clone())
                .collect(),
        )
    }
}

/// Records each shortcut run the broker finishes as one undoable run in the
/// workspace document, from the documents it changed: those open here and
/// those persisted on this device. Driven by the frame pump, never by paint.
#[derive(Default)]
pub(crate) struct ShortcutRunRecorder {
    pending: Vec<PendingShortcutRun>,
    next_look: Option<Instant>,
}

impl ShortcutRunRecorder {
    /// Whether a run waits to be recorded, here or in `broker`. Cheap enough
    /// for every tick.
    pub(crate) fn has_work(&self, broker: Option<&AppActionBroker>) -> bool {
        !self.pending.is_empty() || broker.is_some_and(AppActionBroker::has_finished_shortcut_runs)
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

    /// Take `broker`'s finished runs and record every waiting run whose
    /// changes reached a document here in `tree`'s document, committing it.
    /// A run with no changes found is given up once [`SHORTCUT_RUN_SETTLE`]
    /// has passed since it finished: it changed nothing this device can undo.
    /// Returns whether `tree` was written.
    pub(crate) fn record(
        &mut self,
        broker: Option<&AppActionBroker>,
        tree: &mut WorkspaceTree,
        records: &RecordStore,
        local: &RoomRecords,
        now: Instant,
    ) -> bool {
        if let Some(broker) = broker {
            let taken = broker.take_finished_shortcut_runs();
            if !taken.is_empty() {
                self.next_look = None;
            }
            self.pending.extend(
                taken
                    .into_iter()
                    .map(|finished| PendingShortcutRun::new(finished, now)),
            );
        }
        if self.pending.is_empty() || self.next_look.is_some_and(|at| now < at) {
            return false;
        }
        self.next_look = now.checked_add(SHORTCUT_RUN_LOOK_INTERVAL);
        let open: BTreeSet<RecordId> = records
            .open_records()
            .into_iter()
            .map(|checkout| checkout.id)
            .collect();
        let known = local.known();
        let mut recorded = false;
        for mut run in std::mem::take(&mut self.pending) {
            let room = crate::shell::state::RoomId::parse(&run.finished.room);
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
            let steps = run.look(&candidates, &open, records, local);
            if record_run_steps(&mut tree.doc, &run.finished, &steps).is_some() {
                recorded = true;
                continue;
            }
            let settling = now.saturating_duration_since(run.since) < SHORTCUT_RUN_SETTLE;
            if settling && !run.finished.report.invocations().is_empty() {
                self.pending.push(run);
            } else {
                tracing::debug!(
                    shortcut = %run.finished.shortcut.name,
                    "a finished shortcut run changed no document on this device; not recorded"
                );
            }
        }
        if recorded {
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
fn load_history_map_at<T: serde::de::DeserializeOwned>(
    path: Option<&std::path::Path>,
    key: &str,
) -> HashMap<u64, T> {
    let doc: serde_json::Map<String, serde_json::Value> = path
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    doc.get(key)
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

fn load_history_map<T: serde::de::DeserializeOwned>(key: &str) -> HashMap<u64, T> {
    // A save still on its way to disk is part of what the file holds.
    settle_history_writes();
    load_history_map_at(history_path().as_deref(), key)
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
    let Some(Some(writer)) = HISTORY_WRITER.get() else {
        return;
    };
    let (done, wait) = mpsc::sync_channel(1);
    if writer.send(HistoryWrite::Settle(done)).is_ok() {
        let _ = wait.recv();
    }
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
    use crate::shell::i18n::t;
    use tilo_desktop::activity::morning::{
        MORNING_DRAFT, MORNING_NEXT, MORNING_OPEN, MORNING_REFRESH, MORNING_REMOVE,
        MorningPreferences,
    };
    let choices: HashMap<u64, MorningPreferences> = load_history_map("morning_sources");
    choices
        .get(&workspace.id.raw())
        .map_or_else(Vec::new, |preferences| {
            preferences
                .for_room(&workspace.room.to_string())
                .iter()
                .flat_map(|source| {
                    let app = crate::shell::launcher::display_name_for_app_id(&source.app)
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
        self.review
            .as_ref()
            .ok_or("authenticated task owner unavailable")?
            .submit(request)
    }
    fn poll_task_review(&self, invocation: uuid::Uuid) -> Option<Result<(), String>> {
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
        crate::shell::launcher::display_name_for_app_id(app)
    }
    fn agent_members(&self, workspace: &Workspace) -> Vec<(String, collab_types::role::Role)> {
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
    pub(crate) fn refresh_observed(
        &mut self,
        tree: &WorkspaceTreeHandle,
        records: &RecordStore,
        local: &dyn tilo_desktop::activity::PersistedRecordHistory,
    ) -> u64 {
        let mut observed = ActivityObservation::capture(tree);
        let workspace = observed.active.raw();
        self.observe_workspace(workspace);
        self.refresh(&mut observed);
        self.refresh_records_with_local(&mut observed, records, local);
        workspace
    }

    pub fn new() -> Self {
        let mut pane = tilo_desktop::activity::RoomActivityPane::new();
        pane.set_host(Rc::new(NativeActivityHost::default()));
        Self(pane, None, ShortcutRunRecorder::default())
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
        Self(pane, None, ShortcutRunRecorder::default())
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
        }));
    }
    /// Whether a finished shortcut run waits to be recorded. Cheap: the
    /// frame pump asks every tick before taking any lock.
    pub(crate) fn shortcut_runs_pending(&self) -> bool {
        self.2.has_work(self.1.as_ref())
    }
    /// Record the shortcut runs the broker finished in `tree`'s document,
    /// each as one undoable run; see [`ShortcutRunRecorder::record`].
    pub(crate) fn record_finished_shortcut_runs(
        &mut self,
        tree: &mut WorkspaceTree,
        records: &RecordStore,
        local: &RoomRecords,
        now: Instant,
    ) -> bool {
        self.2.record(self.1.as_ref(), tree, records, local, now)
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
            Native::SpawnFailed { detail } => Shared::SpawnFailed {
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
                "Write draft",
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
        pane.record_finished_shortcut_runs(&mut tree, &records, &local, now);
        pane.record_finished_shortcut_runs(&mut tree, &records, &local, next);

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

    /// A change the "Publish a draft" fixture app makes as a step of `run`.
    fn commit_draft_step(doc: &mut automerge::AutoCommit, run: tilo_automation::RunId) {
        doc.put(automerge::ROOT, "draft", "Launch notes").unwrap();
        let context = ui::tilo_actions::ActionContext::for_resource(
            "Write draft",
            "fixture.drafts:inbox",
            "fixture.drafts",
            "Inbox",
        )
        .with_app("so.tilo.fixture-drafts", "Drafts")
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
        // to record yet, and the run keeps waiting.
        assert!(!pane.record_finished_shortcut_runs(&mut tree, &records, &local, finished_at));
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
        assert!(pane.record_finished_shortcut_runs(&mut tree, &records, &local, next));
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
        assert_eq!(runs[0].steps[0].summary, "Write draft");
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
            assert!(!pane.record_finished_shortcut_runs(&mut tree, &records, &local, at));
            assert!(pane.shortcut_runs_pending());
            at = at
                .checked_add(SHORTCUT_RUN_LOOK_INTERVAL)
                .expect("the next look");
        }
        assert!(!pane.record_finished_shortcut_runs(&mut tree, &records, &local, at));
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
