//! The workspace's activity: the desk's own document ([`crate::desk_doc`]) and the records its apps
//! keep in the store, served to the Activity app and to presence.
//! A record is read away from the loop ([`crate::platform::watched`]) and a change asks the desk for
//! the frame that lists it; what this device has seen of a workspace is its own ([`HistoryMaps`]) and
//! never reaches a shared record.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use automerge::{AutoCommit, LoadOptions, OnPartialLoad};
use durable_store::{DurableStore, StorageKey, WriteOp};
use serde_json::Value;
use tilo_desktop_model::{DesktopTree, TabId, Workspace, WorkspaceId, WorkspaceSlot};
use tilo_desktop_surface::HistorySource;
use tilo_desktop_surface::activity::describer::{Describer, Job};
use tilo_desktop_surface::activity::{
    ActivityHost, ActivitySnapshot, DetachedRecordHistory, HistoryOrigin, TaskRunDocument,
    TaskRunUndo, WorkspaceDeparture, WorkspaceSeenFrontier, morning,
};
use tilo_desktop_surface::history_log;
use tilo_desktop_surface::session::{ActivityRequests, activity_requests};
use tilo_lock::MutexRecover as _;
use ui::record::RecordCheckout;

use crate::activity_provider::ActivityWire;
use crate::desk_doc::{DeskDoc, Kept};
use crate::platform::Store;
use crate::platform::apps::Apps;
use crate::platform::authors::{People, Writers};
use crate::platform::pane::Pane;
#[cfg(target_os = "linux")]
use crate::platform::watched::Watched;
#[cfg(target_os = "linux")]
use tilo_actions::remote::workspace_activity as wire;

#[cfg(target_os = "linux")]
use crate::unlock_history::UnlockHistory;
#[cfg(target_os = "linux")]
use tilo_device_fabric::link::SharedLink;

/// The file this device's view was kept in before it was a record.
const VIEW_FILE: &str = "activity-history.json";

/// The package this device's view of the activity is kept under: the session's.
const VIEW_PACKAGE: &str = "so.tilo.session.activity";

/// Seconds since the epoch, as a change of the desk names its time.
pub(crate) fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
        })
}

/// One device's lasting refusal: its workspace, the device, the class of its reason.
#[cfg(target_os = "linux")]
type ToldRefusal = (String, [u8; 32], neighbour_wire::RefusalReason);

pub struct Given {
    pub desk: DeskDoc,
    /// Who sits here.
    pub person: String,
    pub store: Option<Store>,
    pub state: Option<PathBuf>,
    pub apps: Option<Arc<dyn Apps>>,
    /// Who a principal is, as presence shows them.
    pub people: People,
    pub writers: Writers,
    pub pane: Rc<dyn Pane>,
    /// The model the desk's changes are described by, while this host answers one.
    pub models: Option<Arc<dyn crate::platform::Models>>,
    /// What moves when the records did, for a host that drives its own store.
    pub stamp: Option<Rc<dyn Fn() -> u64>>,
    pub wake: Option<tilo_service::Wake>,
}

/// A workspace's activity, as this session serves it.
#[derive(Clone)]
pub struct Activity(Rc<Inner>);

struct Inner {
    desk: DeskDoc,
    /// What the devices answered when one asked to be unlocked.
    #[cfg(target_os = "linux")]
    unlock: UnlockHistory,
    /// Who sits here.
    person: String,
    maps: HistoryMaps,
    apps: Option<Arc<dyn Apps>>,
    /// Who a principal is, as presence shows them.
    people: People,
    writers: Writers,
    /// Where the activity model's code is.
    pane: Rc<dyn Pane>,
    /// The model the desk's changes are described by, while this host answers one.
    models: Option<Arc<dyn crate::platform::Models>>,
    /// The workspaces this device holds, once the session opened them.
    #[cfg(target_os = "linux")]
    engine: RefCell<Option<Arc<crate::engine::Engine>>>,
    /// The session's action door, once its apps run here.
    #[cfg(target_os = "linux")]
    door: RefCell<Option<Arc<dyn crate::task_undo::UndoDoor>>>,
    /// The records' content this device keeps.
    #[cfg(target_os = "linux")]
    content: RefCell<Option<Arc<crate::content::Plane>>>,
    #[cfg(target_os = "linux")]
    undos: crate::task_undo::Undos,
    /// The records each workspace holds, read on a worker of its own.
    #[cfg(target_os = "linux")]
    records: RefCell<HashMap<String, Rc<Watched<Vec<DetachedRecordHistory>>>>>,
    store: Option<Store>,
    /// Moves whenever the records read for a workspace moved.
    records_moved: Arc<AtomicU64>,
    /// What a host whose store it drives itself answers for "the records moved".
    stamp: Option<Rc<dyn Fn() -> u64>>,
    /// The stamp last read, and the read it made.
    read_at: Cell<Option<(u64, Rc<Vec<DetachedRecordHistory>>)>>,
    /// The desk a change the model wrote back leaves, for the next pass.
    restored: RefCell<Option<DesktopTree>>,
    /// The resources the person opened from the history.
    openings: RefCell<Vec<(WorkspaceId, ui::HistoryResourceTarget)>>,
    /// The windows' end of what they ask of the activity, and what they follow.
    requests: ActivityRequests,
    /// Raised when a window asked, or what the history reads moved.
    woken: Arc<AtomicBool>,
    /// The broker's end of the same activity, once this session serves the apps it runs.
    wire: RefCell<Option<Arc<ActivityWire>>>,
    /// The revision the model last served its reads at, as the apps following them were last told.
    served: Cell<Option<u64>>,
    /// One notice per device and reason, never one per write or per pass.
    #[cfg(target_os = "linux")]
    told_refusals: RefCell<BTreeSet<ToldRefusal>>,
    /// What the desk's history says its changes were, as a model answered it.
    describer: RefCell<Describer>,
    /// Answers the model gave, waiting for the desk's thread to write them.
    described: Arc<Mutex<Vec<(Job, String)>>>,
    /// One request at a time keeps a long history from spending the person's provider on a burst.
    describing: Arc<AtomicBool>,
    /// The desk's changes the describer last read, so a pass with nothing new asks nothing.
    described_heads: RefCell<Vec<automerge::ChangeHash>>,
    /// When a model was last looked for.
    looked_for_model: Cell<Option<Instant>>,
    /// What wakes the session's loop when a model answered off this thread.
    wake: Option<tilo_service::Wake>,
}

/// How often a session with nothing new looks again for a model the person may have configured.
const LOOK_FOR_MODEL: Duration = Duration::from_secs(30);

/// Something the history reads moved: the desk draws a frame, and the loop wakes.
fn ask_for_frame(woken: &AtomicBool, wake: Option<&tilo_service::Wake>) {
    woken.store(true, Ordering::Release);
    if let Some(wake) = wake {
        wake();
    }
}

impl Activity {
    /// This session's activity: the desk under `state` and `old_desk` , the records, the person.
    #[cfg(target_os = "linux")]
    pub(crate) fn new(
        state: Option<&std::path::Path>,
        old_desk: Option<&std::path::Path>,
        person: String,
        store: Option<Store>,
        apps: Option<Arc<dyn Apps>>,
        wake: Option<tilo_service::Wake>,
    ) -> Self {
        let desk = DeskDoc::load(state, old_desk, &person);
        let unlock = UnlockHistory::new(state, &person);
        let given = Given {
            desk,
            person,
            store,
            state: state.map(std::path::Path::to_path_buf),
            apps,
            people: crate::platform::authors::unnamed(),
            writers: Writers::untracked(),
            pane: Rc::new(crate::platform::pane::Linked),
            models: None,
            stamp: None,
            wake,
        };
        Self::assemble(given, unlock)
    }

    /// This session's activity over what `given` names: every host's.
    pub fn over(given: Given) -> Self {
        #[cfg(target_os = "linux")]
        let unlock = UnlockHistory::new(None, &given.person);
        Self::assemble(
            given,
            #[cfg(target_os = "linux")]
            unlock,
        )
    }

    fn assemble(given: Given, #[cfg(target_os = "linux")] unlock: UnlockHistory) -> Self {
        let Given {
            desk,
            person,
            store,
            state,
            apps,
            people,
            writers,
            pane,
            models,
            stamp,
            wake,
        } = given;
        let woken = Arc::new(AtomicBool::new(false));
        let requests = activity_requests(Arc::clone(&woken));
        Self(Rc::new(Inner {
            desk,
            #[cfg(target_os = "linux")]
            unlock,
            maps: HistoryMaps::load(store.clone(), state.as_deref()),
            apps,
            people,
            writers,
            pane,
            models,
            #[cfg(target_os = "linux")]
            engine: RefCell::default(),
            #[cfg(target_os = "linux")]
            door: RefCell::default(),
            #[cfg(target_os = "linux")]
            content: RefCell::default(),
            #[cfg(target_os = "linux")]
            undos: crate::task_undo::Undos::default(),
            person,
            #[cfg(target_os = "linux")]
            records: RefCell::default(),
            store,
            records_moved: Arc::default(),
            stamp,
            read_at: Cell::default(),
            restored: RefCell::default(),
            openings: RefCell::default(),
            requests,
            woken,
            wire: RefCell::default(),
            served: Cell::default(),
            #[cfg(target_os = "linux")]
            told_refusals: RefCell::default(),
            describer: RefCell::default(),
            described: Arc::default(),
            describing: Arc::default(),
            described_heads: RefCell::default(),
            looked_for_model: Cell::default(),
            wake,
        }))
    }

    /// Show where a workspace's writes were refused from `engine` .
    #[cfg(target_os = "linux")]
    fn attach_engine(&self, engine: Arc<crate::engine::Engine>) {
        *self.0.engine.borrow_mut() = Some(engine);
    }

    /// Tell the person about the refusals waiting cannot change, once each until the verdict goes.
    #[cfg(target_os = "linux")]
    fn tell_refusals(&self, notifications: &crate::notifications::SessionNotifications) {
        let Some(engine) = self.0.engine.borrow().as_ref().map(Arc::clone) else {
            return;
        };
        let mut told = self.0.told_refusals.borrow_mut();
        let mut lasting = Vec::new();
        for (workspace, refusal) in engine.lasting_refusals() {
            let key = (workspace.clone(), refusal.by, refusal.reason);
            lasting.push(key.clone());
            if !told.insert(key) {
                continue;
            }
            let label = engine
                .device_labels(&workspace)
                .get(&refusal.by)
                .filter(|label| !label.is_empty())
                .cloned()
                .unwrap_or_else(|| hex::encode(&refusal.by[..4]));
            notifications.tell(crate::notices::refused_write_notice(&label, refusal.reason));
        }
        told.retain(|key| lasting.contains(key));
    }

    /// Open this device's workspaces: what refused a write, and what the devices call themselves.
    #[cfg(target_os = "linux")]
    pub(crate) fn open_engine(
        &self,
        options: &crate::Options,
        handle: &tilo_desktop_surface::DesktopHandle,
    ) -> Option<Arc<crate::engine::Engine>> {
        let engine = crate::options::open_engine(options, handle)?;
        self.attach_engine(Arc::clone(&engine));
        Some(engine)
    }

    /// The desk this device left, and what became of the desk on disk if it did not open it.
    pub fn opened_desk(&self) -> Kept {
        self.0.desk.opened()
    }

    /// What the windows ask of the activity and follow.
    pub fn requests(&self) -> ActivityRequests {
        self.0.requests.clone()
    }

    /// Serve the apps this session runs through `wire` .
    #[cfg(target_os = "linux")]
    pub(crate) fn serve_apps(&self, wire: Arc<ActivityWire>) {
        *self.0.wire.borrow_mut() = Some(wire);
    }

    /// Undo an agent's run in the records it changed, as the person at the screen.
    #[cfg(target_os = "linux")]
    pub(crate) fn attach_door(&self, door: Arc<dyn crate::task_undo::UndoDoor>) {
        *self.0.door.borrow_mut() = Some(door);
    }

    /// Read the output of command runs from `content` .
    #[cfg(target_os = "linux")]
    pub(crate) fn attach_content(&self, content: Arc<crate::content::Plane>) {
        *self.0.content.borrow_mut() = Some(content);
    }

    /// Take part in `link` 's fabric.
    #[cfg(target_os = "linux")]
    pub(crate) fn watch_fabric(&self, link: SharedLink) {
        self.0.unlock.watch(link);
    }

    /// Write what the fabric holds now into the person's own record.
    #[cfg(target_os = "linux")]
    pub(crate) fn keep_unlock(&self) {
        if self.0.unlock.absorb() {
            ask_for_frame(&self.0.woken, self.0.wake.as_ref());
        }
    }

    /// Serve what the activity holds on this pass: the apps following it, and the refusals owed.
    #[cfg(target_os = "linux")]
    pub(crate) fn serve(&self, notifications: &crate::notifications::SessionNotifications) {
        self.serve_following();
        self.tell_refusals(notifications);
    }

    pub fn serve_following(&self) {
        let served = self.0.requests.served_revision();
        if served != self.0.served.get() {
            self.0.served.set(served);
            if let Some(wire) = self.0.wire.borrow().as_ref() {
                wire.changed();
            }
        }
    }

    /// Record the desk as `tree` holds it, at `time` , by the person here. Whether it changed.
    pub fn record_desk(
        &self,
        tree: &DesktopTree,
        time: i64,
        shows: &[(tilo_desktop_model::TabId, String)],
    ) -> bool {
        self.0.desk.record(tree, time, shows)
    }

    /// [`Self::record_desk`], and write even how the desk looks: what ends the session.
    pub fn record_desk_now(
        &self,
        tree: &DesktopTree,
        time: i64,
        shows: &[(tilo_desktop_model::TabId, String)],
    ) -> bool {
        self.0.desk.record_now(tree, time, shows)
    }

    /// The desk document, which the arrangement reads and changes too.
    pub fn desk(&self) -> std::rc::Rc<tilo_session_core::desk::Desk> {
        self.0.desk.desk()
    }

    /// The workspace a change the model wrote back leaves the desk at, if one is waiting.
    pub fn take_restored(&self) -> Option<DesktopTree> {
        self.0.restored.borrow_mut().take()
    }

    /// The resources the person opened from the history, each with its workspace.
    pub fn take_openings(&self) -> Vec<(WorkspaceId, ui::HistoryResourceTarget)> {
        std::mem::take(&mut *self.0.openings.borrow_mut())
    }

    /// Whether a window asked, or what the history reads moved, since last taken.
    pub fn take_woken(&self) -> bool {
        self.0.woken.swap(false, Ordering::AcqRel)
    }

    /// When this session next looks for a model.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.0
            .looked_for_model
            .get()
            .and_then(|at| at.checked_add(LOOK_FOR_MODEL))
    }

    /// Say what the desk's changes were when the desk moved: one request at a time, off the desk's
    /// thread. Whether the document changed.
    pub fn describe(&self) -> bool {
        let mut wrote = false;
        let answers = std::mem::take(&mut *self.0.described.lock_recover());
        {
            let mut describer = self.0.describer.borrow_mut();
            for (job, answer) in answers {
                wrote |= if answer.is_empty() {
                    // No model answered: nothing is described in its place.
                    describer.failed(&job);
                    false
                } else {
                    self.0
                        .desk
                        .write_metadata(|doc| describer.apply(doc, &job, &answer))
                };
            }
        }
        if self.0.describing.load(Ordering::Acquire) {
            return wrote;
        }
        let heads = self.0.desk.heads();
        let look_again = self
            .0
            .looked_for_model
            .get()
            .is_none_or(|at| at.elapsed() >= LOOK_FOR_MODEL);
        if heads == *self.0.described_heads.borrow() && !look_again {
            return wrote;
        }
        *self.0.described_heads.borrow_mut() = heads;
        let mut describer = self.0.describer.borrow_mut();
        describer.set_enabled(self.0.models.is_some());
        let Some(job) = describer.next(&mut self.0.desk.document()) else {
            self.0.looked_for_model.set(Some(Instant::now()));
            return wrote;
        };
        drop(describer);
        self.0.looked_for_model.set(Some(Instant::now()));
        self.0.describing.store(true, Ordering::Release);
        let answers = Arc::clone(&self.0.described);
        let describing = Arc::clone(&self.0.describing);
        let woken = Arc::clone(&self.0.woken);
        let wake = self.0.wake.clone();
        let models = self.0.models.clone();
        let ask = move || async move {
            let answer = crate::activity_models::answer(models.as_deref(), &job).await;
            answers.lock_recover().push((job, answer));
            describing.store(false, Ordering::Release);
            // The desk draws a frame to write the answer where it belongs.
            ask_for_frame(&woken, wake.as_ref());
        };
        // A page has one thread and nothing may block on it: the answer is a task of its own turn.
        #[cfg(target_arch = "wasm32")]
        tilo_rt::spawn_task(ask());
        #[cfg(not(target_arch = "wasm32"))]
        std::thread::spawn(move || {
            if let Ok(runtime) = tilo_rt::try_app() {
                let _ = runtime.block_on(ask());
            }
        });
        wrote
    }

    /// The `WorkspaceId` of `workspace` , as `tree` holds it.
    fn id_of(tree: &DesktopTree, workspace: WorkspaceSlot) -> Option<WorkspaceId> {
        tree.workspaces
            .iter()
            .find(|held| held.slot == workspace)
            .map(Workspace::id)
    }

    /// The records `workspace_id` holds, as read for the workspace and, on a desktop, followed.
    fn records_of(&self, workspace_id: WorkspaceId) -> Vec<DetachedRecordHistory> {
        let Some(store) = self.0.store.clone() else {
            return Vec::new();
        };
        let key = workspace_id.to_string();
        // A host that drives its own store says when it moved: the read is made there and then,
        // and kept until it moves again.
        if let Some(stamp) = &self.0.stamp {
            let now = stamp();
            if let Some((at, read)) = self.0.read_at.take() {
                let held = Rc::clone(&read);
                self.0.read_at.set(Some((at, read)));
                if at == now {
                    return held.as_ref().clone();
                }
            }
            let read = Rc::new(read_workspace_records(
                store.as_ref(),
                &key,
                &self.0.writers.noted,
            ));
            self.0.read_at.set(Some((now, Rc::clone(&read))));
            return read.as_ref().clone();
        }
        #[cfg(target_os = "linux")]
        if let Some(known) = self.0.records.borrow().get(&key) {
            return known.get().as_ref().clone();
        }
        #[cfg(target_os = "linux")]
        {
            let directories = record_paths(store.as_ref(), &key);
            let reading = store.clone();
            let by_workspace = key.clone();
            let noted = crate::platform::authors::new_noted({
                let noted = self.0.writers.noted.clone();
                move |actor| noted(actor)
            });
            let read = move || read_workspace_records(reading.as_ref(), &by_workspace, &noted);
            let Ok(watch) = Watched::start("tilo-activity-records", directories, read) else {
                return read_workspace_records(store.as_ref(), &key, &self.0.writers.noted);
            };
            let moved = Arc::clone(&self.0.records_moved);
            // The record was read again because it changed: the model reading it is told, so an
            // open history lists the change with no one asking.
            let woken = Arc::clone(&self.0.woken);
            let wake = self.0.wake.clone();
            watch.on_change(move || {
                moved.fetch_add(1, Ordering::AcqRel);
                ask_for_frame(&woken, wake.as_ref());
            });
            let watch = Rc::new(watch);
            self.0.records.borrow_mut().insert(key, Rc::clone(&watch));
            watch.get().as_ref().clone()
        }
        #[cfg(not(target_os = "linux"))]
        Vec::new()
    }
}

impl HistorySource for Activity {
    fn workspace_history(
        &mut self,
        tree: &DesktopTree,
        workspace: WorkspaceSlot,
    ) -> Option<ActivitySnapshot> {
        Self::id_of(tree, workspace)?;
        // The desk as it stands is the newest change of it: a workspace made this pass is in the
        // document the pane reads, not only in the tree.
        self.record_desk(tree, now_seconds(), &[]);
        let mut snapshot = ActivitySnapshot::of_desktop(tree.clone(), self.0.desk.document());
        snapshot.desktop.active = workspace;
        let restored = Rc::downgrade(&self.0);
        let commit = Rc::new(move |tree: &DesktopTree| {
            let Some(inner) = restored.upgrade() else {
                return;
            };
            *inner.restored.borrow_mut() = Some(tree.clone());
        });
        Some(snapshot.committing(commit))
    }

    fn record_histories(
        &mut self,
        tree: &DesktopTree,
        workspace: WorkspaceSlot,
    ) -> Vec<DetachedRecordHistory> {
        let mut records = Vec::new();
        if let Some(workspace_id) = Self::id_of(tree, workspace) {
            records.extend(self.records_of(workspace_id));
        }
        // What the person's devices answered when one of them asked to be unlocked is theirs and
        // not any one workspace's: it reads the same in every workspace's history.
        #[cfg(target_os = "linux")]
        if let Some(record) = self.0.unlock.history() {
            records.push(record);
        }
        records
    }

    fn host(&self) -> Rc<dyn ActivityHost> {
        Rc::new(Host(Rc::clone(&self.0))) as Rc<dyn ActivityHost>
    }

    fn register_authors(&self, authors: &mut ui::AuthorDirectory) {
        // Who this host's own mounts write as: a record's changes by the person who made them.
        (self.0.writers.register)(authors);
        // The names the desk's changes are known by: the person here, and whoever the desk came
        // from when it was read into its document.
        let known = self.0.desk.authors();
        let people = &*self.0.people;
        history_log::register_authors(authors, &known, people);
        // The changes this device made are the person's own: they read as presence shows them here,
        // not as another participant.
        let mine = self.0.person.clone();
        for (actor, author) in &known {
            if *author == mine {
                authors.register_local_identity(actor);
            }
        }
        // The devices that answered a boot are named as their own snapshots name them, and this one
        // of them is the person here.

        #[cfg(target_os = "linux")]
        {
            let unlock = &self.0.unlock;
            history_log::register_authors(authors, &unlock.authors(), people);
            if let Some(actor) = unlock.local_actor() {
                authors.register_local_identity(&actor);
            }
        }
    }

    fn revision(&self) -> u64 {
        use std::hash::{Hash as _, Hasher as _};
        #[cfg(target_os = "linux")]
        self.0.unlock.absorb();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.0.desk.heads().hash(&mut hasher);
        #[cfg(target_os = "linux")]
        self.0.unlock.heads().hash(&mut hasher);
        if let Some(stamp) = &self.0.stamp {
            stamp().hash(&mut hasher);
        }
        self.0
            .records_moved
            .load(Ordering::Acquire)
            .hash(&mut hasher);
        hasher.finish()
    }

    fn activity_pane(&self) -> tilo_desktop_surface::ActivityPaneCode {
        self.0.pane.pane_code()
    }

    /// The person opened a resource from the history: its app opens on the next pass.
    fn resource_tab(
        &self,
        tree: &DesktopTree,
        target: &ui::HistoryResourceTarget,
    ) -> Option<tilo_desktop_model::TabId> {
        // No app holds the person's own record of what their devices answered: its rows are read in
        // the history, and opened nowhere.
        #[cfg(target_os = "linux")]
        if self.0.unlock.holds(&target.id) {
            return None;
        }
        let workspace_id = tree.active_workspace().map(Workspace::id)?;
        self.0
            .openings
            .borrow_mut()
            .push((workspace_id, target.clone()));
        None
    }
}

/// The directories a workspace's records change in: its index and each record's.
#[cfg(target_os = "linux")]
fn record_paths(store: &dyn DurableStore, workspace: &str) -> Vec<PathBuf> {
    let mut directories = BTreeSet::new();
    for (package, index_room) in workspace_packages(store, workspace) {
        for key in [StorageKey::workspace_index(&package, &index_room)]
            .into_iter()
            .chain(record_keys(store, &package, &index_room))
        {
            if let Some(directory) = store
                .local_path(&key)
                .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
            {
                directories.insert(directory);
            }
        }
    }
    directories.into_iter().collect()
}

/// The packages that hold records in `workspace` , as their apps' indexes name them.
fn workspace_packages(store: &dyn DurableStore, workspace: &str) -> Vec<(String, String)> {
    match store.workspace_indexes() {
        Ok(indexes) => indexes
            .into_iter()
            .filter(|(_, room)| room == workspace)
            .collect(),
        Err(error) => {
            tracing::warn!(%error, "the workspace indexes of this device are not listed");
            Vec::new()
        }
    }
}

/// Every record `package` holds in `workspace`, as the store has it.
#[cfg(target_os = "linux")]
fn record_keys(store: &dyn DurableStore, package: &str, workspace: &str) -> Vec<StorageKey> {
    match store.records(package, workspace) {
        Ok(records) => records
            .into_iter()
            .map(|record| StorageKey::record(package, workspace, record))
            .collect(),
        Err(error) => {
            tracing::warn!(%error, package, %workspace, "a workspace's records are not listed");
            Vec::new()
        }
    }
}

/// Every record a workspace holds, with the document its app last wrote.
fn read_workspace_records(
    store: &dyn DurableStore,
    workspace: &str,
    noted: &crate::platform::authors::Noted,
) -> Vec<DetachedRecordHistory> {
    let mut read = Vec::new();
    let mut seen = BTreeSet::new();
    for (package, index_room) in workspace_packages(store, workspace) {
        let Some(checkouts) = named_in(store, &package, &index_room) else {
            continue;
        };
        for checkout in checkouts {
            let id = checkout.id.as_str().to_owned();
            if !seen.insert((package.clone(), id.clone())) {
                continue;
            }
            let document = document_of(store, &package, workspace, &id);
            if let Some(document) = &document {
                noted(document.get_actor());
            }
            read.push(DetachedRecordHistory {
                app: package.clone(),
                checkout,
                document,
            });
        }
    }
    read
}

/// The records `package` 's index for `workspace` names; `None` when it holds no index there.
fn named_in(
    store: &dyn DurableStore,
    package: &str,
    workspace: &str,
) -> Option<Vec<RecordCheckout>> {
    let key = StorageKey::workspace_index(package, workspace);
    let stored = match store.read(&key) {
        Ok(stored) => stored?,
        Err(error) => {
            tracing::warn!(%error, package, %workspace, "a workspace's index is not read");
            return None;
        }
    };
    if stored.bytes.is_empty() {
        return None;
    }
    // An app keeping the index appends to it, and this read may land on a tail it has not finished
    // writing: the changes before it are the index as of the last write that completed.
    let options = LoadOptions::new().on_partial_load(OnPartialLoad::Ignore);
    match AutoCommit::load_with_options(&stored.bytes, options) {
        Ok(doc) => Some(ui::record::index::discoverable_references(&doc)),
        Err(error) => {
            tracing::warn!(%error, package, %workspace, "a workspace's index does not load");
            None
        }
    }
}

/// The document `package` keeps for `record` , as its app last committed it.
fn document_of(
    store: &dyn DurableStore,
    package: &str,
    workspace: &str,
    record: &str,
) -> Option<AutoCommit> {
    let stored = match store.read(&StorageKey::record(package, workspace, record)) {
        Ok(stored) => stored?,
        Err(error) => {
            tracing::warn!(%error, %package, %record, "a record is not read");
            return None;
        }
    };
    if stored.bytes.is_empty() {
        return None;
    }
    let options = LoadOptions::new().on_partial_load(OnPartialLoad::Ignore);
    match AutoCommit::load_with_options(&stored.bytes, options) {
        Ok(doc) => Some(doc),
        Err(error) => {
            tracing::warn!(%error, %package, %record, "a record does not load");
            None
        }
    }
}

/// What the model asks of the session, over the one state this activity keeps.
struct Host(Rc<Inner>);

impl ActivityHost for Host {
    fn local_user(&self) -> String {
        self.0.person.clone()
    }

    fn local_author(&self, authors: &mut ui::AuthorDirectory, actor: &str) -> Option<String> {
        if authors.register_local_identity(actor) {
            return None;
        }
        (!self.0.person.is_empty()).then(|| self.0.person.clone())
    }

    fn load_morning_sources(&self) -> HashMap<u64, morning::MorningPreferences> {
        self.0.maps.map("morning_sources")
    }

    fn save_morning_sources(&self, sources: &HashMap<u64, morning::MorningPreferences>) {
        self.0.maps.save("morning_sources", sources);
    }

    fn load_frontiers(&self) -> HashMap<u64, WorkspaceSeenFrontier> {
        self.0.maps.map("seen_frontiers")
    }

    fn save_frontiers(&self, frontiers: &HashMap<u64, WorkspaceSeenFrontier>) {
        self.0.maps.save("seen_frontiers", frontiers);
    }

    fn load_departures(&self) -> HashMap<u64, WorkspaceDeparture> {
        self.0.maps.map("departures")
    }

    fn save_departures(&self, departures: &HashMap<u64, WorkspaceDeparture>) {
        self.0.maps.save("departures", departures);
    }

    fn mutations_available(&self) -> bool {
        true
    }

    /// The desk's own layout is this session's to write; a record's history is its app's.
    fn writable(&self, origin: &HistoryOrigin) -> bool {
        matches!(origin, HistoryOrigin::DesktopTree)
    }

    #[cfg(target_os = "linux")]
    fn record_content(&self, workspace: &str, app: &str, hash: &str) -> Option<Vec<u8>> {
        self.0.content.borrow().as_ref()?.read(workspace, app, hash)
    }

    fn app_display_name(&self, app: &str) -> Option<String> {
        let name = self.0.apps.as_ref()?.name(app);
        (!name.is_empty()).then_some(name)
    }

    #[cfg(target_os = "linux")]
    fn refusals(&self, workspace: &str) -> Vec<wire::Refusal> {
        let Some(engine) = self.0.engine.borrow().as_ref().map(Arc::clone) else {
            return Vec::new();
        };
        let labels = engine.device_labels(workspace);
        engine
            .write_refusals(workspace)
            .into_iter()
            .flat_map(|refusal| {
                let label = labels
                    .get(&refusal.by)
                    .filter(|label| !label.is_empty())
                    .cloned()
                    .unwrap_or_else(|| hex::encode(&refusal.by[..4]));
                let reason = crate::notices::reason_in_words(refusal.reason);
                let permanent = refusal.reason.permanent();
                refusal
                    .changes
                    .into_iter()
                    .map(|change| wire::Refusal {
                        change: wire::change_hex(&change),
                        device: hex::encode(refusal.by),
                        label: label.clone(),
                        reason: reason.clone(),
                        permanent,
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    fn undo_task_run(&self, undo: TaskRunUndo) -> Result<(), String> {
        // Undone by each changed document's own history, as the person at the screen.
        #[cfg(target_os = "linux")]
        {
            let Some(door) = self.0.door.borrow().as_ref().map(Arc::clone) else {
                return Err(tilo_desktop_surface::i18n::t(
                    "activity-undo-task-unavailable",
                ));
            };
            let woken = Arc::clone(&self.0.woken);
            let wake = self.0.wake.clone();
            self.0.undos.start(
                door,
                self.0.person.clone(),
                undo.workspace,
                undo.invocation,
                undo.task_id,
                undo.steps,
                move || ask_for_frame(&woken, wake.as_ref()),
            )
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = undo;
            Err(tilo_desktop_surface::i18n::t(
                "activity-undo-task-unavailable",
            ))
        }
    }

    fn poll_task_run_undo(
        &self,
        invocation: uuid::Uuid,
    ) -> Option<Result<Vec<TaskRunDocument>, String>> {
        #[cfg(target_os = "linux")]
        {
            match self.0.undos.poll(invocation)? {
                crate::task_undo::UndoState::Running => None,
                crate::task_undo::UndoState::Ended(outcome) => Some(outcome),
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = invocation;
            None
        }
    }
}

/// One step the person took, as presence reports it to the people they are with: the titles of what
/// they did, never the content.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct OwnAction {
    /// The step's history entry id, 32 hex.
    pub id: String,
    /// What the step did, e.g. "Edited the text".
    pub action: String,
    /// The record's title.
    pub resource: String,
    /// The app's name.
    pub app: String,
    pub at_ms: u64,
}

/// The most of its own steps a device reports as what its person is doing.
const MAX_OWN_ACTIONS: usize = 16;

/// The bound, in characters, of each title reported.
const MAX_OWN_TITLE_CHARS: usize = 96;

impl Activity {
    /// What `author` did in `workspace`'s records since `since_ms` (Unix milliseconds), oldest
    /// first: the titles of their own steps, never the content. A run of the same step on the same
    /// resource — typing commits one a second — is one thing they did.
    pub fn own_actions(
        &self,
        tree: &DesktopTree,
        workspace: WorkspaceSlot,
        author: &str,
        since_ms: u64,
    ) -> Vec<OwnAction> {
        let Some(workspace_id) = Self::id_of(tree, workspace) else {
            return Vec::new();
        };
        let shown: HashMap<String, TabId> = self
            .0
            .desk
            .desk()
            .records()
            .into_iter()
            .map(|(tab, record)| (record, tab))
            .collect();
        let titles: HashMap<TabId, &str> = tree
            .tabs()
            .map(|tab| (tab.id, tab.title.as_str()))
            .collect();
        let mut authors = ui::AuthorDirectory::new();
        (self.0.writers.register)(&mut authors);
        authors.set_reader(author);
        let mut actions = Vec::new();
        for record in self.records_of(workspace_id) {
            let Some(mut doc) = record.document else {
                continue;
            };
            let entries = ui::HistorySource::new()
                .without_record_genesis()
                .with_authors(authors.clone())
                .with_max_entries(MAX_OWN_ACTIONS)
                .entries(&mut doc);
            let app_name = self
                .0
                .apps
                .as_ref()
                .map_or_else(String::new, |apps| apps.name(&record.app));
            let tab = shown.get(record.checkout.id.as_str()).copied();
            let in_tree = record.checkout.id.tree().is_some();
            for entry in entries {
                let mine = entry.author.relation == ui::widget::AuthorRelation::Reader
                    || entry.author.name == author;
                let at_ms = entry
                    .timestamp
                    .duration_since(ui::time::UNIX_EPOCH)
                    .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
                if !mine || at_ms < since_ms || !is_doing(&entry) {
                    continue;
                }
                let (action, resource) = match &entry.context {
                    Some(context) => (
                        ui::history_source::sentence::step_title(context),
                        if in_tree {
                            shared_part(context)
                        } else {
                            context.resource.as_ref().map(|r| r.label.clone())
                        },
                    ),
                    None => (None, None),
                };
                let action = bounded_title(&action.unwrap_or_else(|| "Edited".to_owned()));
                if action.is_empty() {
                    continue;
                }
                let resource = resource
                    .filter(|label| !label.trim().is_empty())
                    .or_else(|| {
                        tab.and_then(|tab| titles.get(&tab))
                            .map(|title| (*title).to_owned())
                    })
                    .unwrap_or_default();
                actions.push(OwnAction {
                    id: hex::encode(entry.id.0),
                    action,
                    resource: bounded_title(&resource),
                    app: bounded_title(&app_name),
                    at_ms,
                });
            }
        }
        actions.sort_by(|a, b| a.at_ms.cmp(&b.at_ms).then_with(|| a.id.cmp(&b.id)));
        actions.dedup_by(|a, b| a.id == b.id);
        actions.reverse();
        actions.dedup_by(|older, newer| {
            older.action == newer.action
                && older.resource == newer.resource
                && older.app == newer.app
        });
        actions.reverse();
        actions.truncate(MAX_OWN_ACTIONS);
        actions
    }
}

/// `text` as a title a person is shown: one line, bounded.
fn bounded_title(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(MAX_OWN_TITLE_CHARS)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// The part of a window's tree a step went to, named where everyone the window is shared with sees
/// it too: the place its app recorded on the step ([`tilo_actions::PLACE_FACET`], a post's channel),
/// or a channel it was done to. A private part is never named.
fn shared_part(context: &tilo_actions::ActionContext) -> Option<String> {
    if let Some(place) = context
        .facets
        .iter()
        .find(|facet| facet.key == tilo_actions::PLACE_FACET)
        .map(|facet| facet.value.trim())
        .filter(|place| !place.is_empty())
    {
        return Some(place.to_owned());
    }
    let resource = context.resource.as_ref()?;
    let label = resource.label.trim();
    if label.is_empty() || !resource.kind.ends_with(".channel") {
        return None;
    }
    Some(if label.starts_with('#') {
        label.to_owned()
    } else {
        format!("#{label}")
    })
}

/// Whether `entry` is something its author did: housekeeping an app does on its own, and content
/// from outside, are not what someone is doing.
fn is_doing(entry: &ui::widget::HistoryEntry) -> bool {
    entry
        .context
        .as_ref()
        .is_none_or(tilo_actions::ActionContext::is_activity)
}

struct HistoryMaps {
    known: RefCell<serde_json::Map<String, Value>>,
    store: Option<Store>,
    /// The claim on the record, held for as long as this session writes it.
    lease: RefCell<Option<durable_store::Lease>>,
}

impl HistoryMaps {
    fn load(store: Option<Store>, state: Option<&std::path::Path>) -> Self {
        let known = store
            .as_ref()
            .and_then(|store| Self::read(store.as_ref()))
            .or_else(|| state.map(Self::kept_in).unwrap_or_default())
            .unwrap_or_default();
        let maps = Self {
            known: RefCell::new(known),
            store,
            lease: RefCell::default(),
        };
        // What the file held is part of the record from now on: the file is left where it is.
        if !maps.known.borrow().is_empty() {
            maps.keep();
        }
        maps
    }

    /// What this device's store holds of the view.
    fn read(store: &dyn DurableStore) -> Option<serde_json::Map<String, Value>> {
        let key = StorageKey::package(VIEW_PACKAGE);
        let stored = match store.read(&key) {
            Ok(stored) => stored?,
            Err(error) => {
                tracing::warn!(%error, "the remembered history view is not read");
                return None;
            }
        };
        serde_json::from_slice(&stored.bytes)
            .inspect_err(
                |error| tracing::warn!(%error, "the remembered history view does not read back"),
            )
            .ok()
    }

    /// The map the file under `state` held, before this device kept it in its store.
    fn kept_in(state: &std::path::Path) -> Option<serde_json::Map<String, Value>> {
        let path = state.join("tilo").join(VIEW_FILE);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "the remembered history map view could not be read");
                return None;
            }
        };
        serde_json::from_slice(&bytes)
            .inspect_err(|error| tracing::warn!(%error, "the remembered history map view does not read back"))
            .ok()
    }

    /// `key` 's per-workspace map, as this session last kept it.
    fn map<T: serde::de::DeserializeOwned>(&self, key: &str) -> HashMap<u64, T> {
        self.known
            .borrow()
            .get(key)
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(serde_json::Map::iter)
            .filter_map(|(id, value)| {
                Some((
                    id.parse().ok()?,
                    serde_json::from_value(value.clone()).ok()?,
                ))
            })
            .collect()
    }

    /// Keep `values` as `key` 's map, for this session and the next.
    fn save<T: serde::Serialize>(&self, key: &str, values: &HashMap<u64, T>) {
        let map = Value::Object(
            values
                .iter()
                .filter_map(|(id, value)| Some((id.to_string(), serde_json::to_value(value).ok()?)))
                .collect(),
        );
        self.known.borrow_mut().insert(key.to_owned(), map);
        self.keep();
    }

    /// Write the view to this device's store, as it stands now.
    fn keep(&self) {
        let Some(store) = &self.store else {
            return;
        };
        let key = StorageKey::package(VIEW_PACKAGE);
        let Ok(bytes) = serde_json::to_vec(&*self.known.borrow()) else {
            return;
        };
        let mut lease = self.lease.borrow_mut();
        if lease.is_none() {
            match store.lease(&key) {
                Ok(held) => *lease = Some(held),
                Err(error) => {
                    tracing::warn!(%error, "the remembered history view is not written");
                    return;
                }
            }
        }
        if let Err(error) = store.write(&key, WriteOp::Replace(bytes), None) {
            tracing::warn!(%error, "the remembered history view is not written");
        }
    }
}

#[cfg(test)]
mod tests {
    //! A native session's history, as the desk serves it: the activity a person opens from its
    //! command is the workspace's own changes, and what they restore from it reaches the desk.

    use super::*;

    use tilo_actions::InvocationId;
    use tilo_actions::remote::workspace_activity as wire;
    use tilo_actions::remote::{Caller, InvokeArgument, PendingPoll};
    use tilo_desktop_model::commands::DesktopCommand;
    use tilo_desktop_model::{IdMinter, Tab, TileContent};
    use tilo_desktop_surface::navigation::{ActivityView, ChromeEffect};
    use tilo_desktop_surface::{DesktopHandle, DesktopSurface, TileSource, seed_tree};
    use ui::widget::TileHost;

    /// What a test desk's tabs say. Nothing the session would start.
    struct Tiles;

    impl TileSource for Tiles {
        fn tile(&mut self, _workspace: &Workspace, tab: &Tab) -> TileHost {
            TileHost::new(ui::widget::TileContent::Notice {
                title: tab.title.clone(),
                detail: String::new(),
            })
        }
    }

    /// The person at this desk.
    fn person() -> Caller {
        Caller::local_person("ada").in_workspace("workspace")
    }

    /// A desk of one workspace with Notes open, its activity served as this session serves it.
    struct Desk {
        surface: DesktopSurface,
        handle: DesktopHandle,
        activity: Activity,
        workspace: WorkspaceSlot,
    }

    impl Desk {
        fn new() -> Self {
            let mut minter = IdMinter::new(7);
            let notes = Tab {
                id: minter.mint(),
                title: "Notes".into(),
                tile: TileContent::LocalApp {
                    app_id: "so.tilo.notes".into(),
                    label: "Notes".into(),
                    note: String::new(),
                },
                app_title: None,
                view: String::new(),
                pinned: false,
            };
            let tree = seed_tree(&mut minter, "Studio", vec![notes]);
            let workspace = tree.active;
            let (mut surface, handle) = DesktopSurface::new(tree, 7, Box::new(Tiles));
            let activity = Activity::new(None, None, "ada".to_string(), None, None, None);
            surface.set_history_source(Box::new(activity.clone()));
            surface.serve_activity(activity.requests());
            Self {
                surface,
                handle,
                activity,
                workspace,
            }
        }

        /// One pass of the desk: the desk as it stood is recorded, then what the windows asked is
        /// answered.
        fn frame(&mut self) {
            let tree = self.handle.read(Clone::clone);
            self.activity.record_desk(&tree, now_seconds(), &[]);
            ui::Widget::update(&mut self.surface, &AutoCommit::new(), &ui::automerge::ROOT);
        }

        /// The person asks for this workspace's history, and it opens.
        fn open_history(&mut self) {
            self.handle
                .dispatch(ChromeEffect::Activity {
                    workspace: self.workspace.raw().to_string(),
                    view: ActivityView::Explore,
                    author: None,
                })
                .expect("the history is admitted");
            self.frame();
        }

        /// The person renames the workspace in front.
        fn rename(&self, name: &str) {
            self.handle
                .apply_command(&DesktopCommand::RenameWorkspace {
                    name: name.to_owned(),
                })
                .expect("the workspace takes the name");
        }

        /// Follow the history as the person's app does.
        fn follow(&self) -> InvocationId {
            let invocation = InvocationId::new();
            let InvokeArgument::Json(query) = wire::argument(&wire::FeedQuery::default()) else {
                panic!("the feed takes a query");
            };
            self.activity
                .requests()
                .follow(&person(), invocation, wire::FEED, query)
                .expect("the feed is followed");
            invocation
        }

        /// What the app following `invocation` last read.
        fn feed(&self, invocation: &InvocationId) -> wire::Feed {
            let read = self
                .activity
                .requests()
                .served(invocation)
                .expect("the history is served")
                .expect("a feed");
            serde_json::from_value(read).expect("a feed")
        }

        /// What the desk's workspace in front is called.
        fn name(&self) -> String {
            self.handle.read(|tree| {
                tree.active_workspace()
                    .map(|workspace| workspace.name.clone())
                    .unwrap_or_default()
            })
        }
    }

    /// The activity opens from its command and lists the workspace's real changes.
    #[test]
    fn the_activity_pane_opens_from_its_command_and_lists_a_real_edit() {
        let mut desk = Desk::new();
        desk.open_history();
        let following = desk.follow();
        desk.frame();
        assert_eq!(
            desk.feed(&following).entries.len(),
            0,
            "the desk as it was opened is not a change"
        );

        desk.rename("Plans");
        desk.frame();
        let feed = desk.feed(&following);
        assert_eq!(feed.entries.len(), 1, "{:?}", feed.entries);
        assert!(
            feed.entries[0].summary.contains("Plans"),
            "the rename is what happened: {:?}",
            feed.entries[0]
        );
        // Whoever wrote it, a row names the person reading it as such: the wire says the change is
        // the reader's own, and words it as every history row does.
        assert_eq!(
            feed.entries[0].author.relation,
            wire::Relation::Reader,
            "and who did it: the person here"
        );
        assert_eq!(
            feed.entries[0].author.name, "You",
            "a row names the person reading it, not by their account's name"
        );
        assert!(
            feed.mutations,
            "the desk's own layout is the person's to undo"
        );
    }

    /// Nothing moved, so no pass asks for a frame: a change says when the model is read again.
    #[test]
    fn a_pass_with_nothing_new_asks_for_no_frame() {
        let mut desk = Desk::new();
        desk.open_history();
        let _following = desk.follow();
        assert!(
            desk.activity.take_woken(),
            "the window asking for its history asks for the frame that serves it"
        );
        for _ in 0..3 {
            desk.frame();
        }
        assert!(
            !desk.activity.take_woken(),
            "nothing moved, so nothing asks for a frame"
        );
    }

    /// An arrangement action is one step the person takes back, staged for the next pass.
    #[test]
    fn an_arrangements_action_is_one_step_the_person_takes_back() {
        let mut desk = Desk::new();
        desk.open_history();
        desk.rename("Plans");
        desk.frame();
        assert_eq!(desk.name(), "Plans");

        // An agent moves the desk as the person asked it to: the change it made is recorded as that
        // action, not as a change of the desk's own.
        desk.rename("Lounge");
        let tree = desk.handle.read(Clone::clone);
        let asked = tilo_actions::ActionContext {
            invoker: Some(tilo_actions::ActionInvoker::actor(
                "agent-task:reading".to_owned(),
            )),
            what: Some(tilo_actions::Did::Verb(tilo_actions::VerbCall::new(
                tilo_actions::ActionKey::new("so.tilo.session", "arrangement", "move"),
                std::iter::empty(),
            ))),
            ..tilo_actions::ActionContext::default()
        };
        let document = desk.activity.desk();
        assert!(
            document.record_action(&tree, now_seconds(), &asked, &[]),
            "the desk keeps the move"
        );
        assert_eq!(
            document.last_step("ada"),
            Some(("agent-task:reading".to_owned(), "move".to_owned())),
            "the step names who asked and what they did"
        );

        // The person takes it back: the move is undone, and the rename before it is still there —
        // the two are separate steps.
        assert!(document.undo("ada"), "the person takes it back");
        let restored = document.tree().expect("the desk the step took it back to");
        assert_eq!(
            restored
                .active_workspace()
                .map(|workspace| workspace.name.as_str()),
            Some("Plans"),
            "exactly the move is undone"
        );
        assert!(document.undo("ada"), "the rename before it is its own step");
        let restored = document
            .tree()
            .expect("and the desk before it is there too");
        assert_eq!(
            restored
                .active_workspace()
                .map(|workspace| workspace.name.as_str()),
            Some("Studio"),
            "the desk the session started on"
        );
    }

    /// A version restored from the history reaches the desk, as a change of it.
    #[test]
    fn a_restore_from_the_history_writes_the_desk_back() {
        let mut desk = Desk::new();
        desk.open_history();
        let following = desk.follow();
        desk.rename("Plans");
        desk.frame();
        desk.rename("Lounge");
        desk.frame();
        assert_eq!(desk.name(), "Lounge");

        let feed = desk.feed(&following);
        let plans = feed
            .entries
            .iter()
            .find(|entry| entry.summary.contains("Plans"))
            .expect("the rename to Plans is in the history")
            .id
            .clone();
        let mut restored = desk.activity.requests().act(
            wire::RESTORE,
            match serde_json::json!(wire::Range::one(&plans)).as_object() {
                Some(range) => range.clone(),
                None => panic!("a range"),
            },
        );
        desk.frame();
        assert!(
            matches!(restored.poll_ready(), PendingPoll::Ready(Ok(()))),
            "the history takes the restore"
        );
        let desk_restored = desk
            .activity
            .take_restored()
            .expect("the model writes the desk back");
        desk.handle.replace_tree(desk_restored);
        assert_eq!(desk.name(), "Plans", "the desk is at the version restored");

        // The restored desk is recorded like any other change of it.
        desk.frame();
        let feed = desk.feed(&following);
        assert!(
            feed.entries.len() > 2,
            "the restore is a change on top: {:?}",
            feed.entries
        );
    }

    /// What a person is doing, as the people here are told it, is their work: an app introducing
    /// them to a chat under the name they arrived with is its housekeeping, not something they did.
    #[test]
    fn housekeeping_and_outside_content_are_not_what_someone_is_doing() {
        use tilo_actions::{ActionContext, ChangeKind};
        use ui::widget::{ArtifactKind, EntryKind, HistoryAuthor, HistoryEntry, HistoryEntryId};
        let entry = |context: Option<ActionContext>| HistoryEntry {
            id: HistoryEntryId([0; 16]),
            author: HistoryAuthor::new("Ada", 120.0, "deadbeef"),
            entry_kind: EntryKind::Details,
            artifact_kind: ArtifactKind::Note,
            timestamp: ui::time::UNIX_EPOCH,
            summary: String::new(),
            text_delta: None,
            diff_payload: None,
            redaction: None,
            context,
        };
        assert!(is_doing(&entry(None)));
        assert!(is_doing(&entry(Some(ActionContext::default()))));
        for kind in [ChangeKind::Housekeeping, ChangeKind::External] {
            assert!(
                !is_doing(&entry(Some(ActionContext::default().with_change(kind)))),
                "{kind:?}"
            );
        }
    }

    /// A step in a window's tree is named by the place its app recorded (a post's channel), else a
    /// channel it was done to; anything else (a direct conversation) is not named at all.
    #[test]
    fn a_step_is_named_by_the_shared_place_it_went_to() {
        let post = tilo_actions::ActionContext::for_resource("chain", "chat.messages", "Messages")
            .with_facet(tilo_actions::ActionFacet::new(
                tilo_actions::PLACE_FACET,
                "#general",
            ));
        assert_eq!(shared_part(&post).as_deref(), Some("#general"));
        let channel = tilo_actions::ActionContext::for_resource("c1", "chat.channel", "design");
        assert_eq!(shared_part(&channel).as_deref(), Some("#design"));
        let direct =
            tilo_actions::ActionContext::for_resource("chain", "chat.messages", "Messages");
        assert_eq!(shared_part(&direct), None);
    }
}
