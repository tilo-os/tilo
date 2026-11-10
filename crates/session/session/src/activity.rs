//! The workspace's activity: the desk's own document ([`crate::desk_doc`]) and the records its apps
//! keep in the store, served to the Activity app and to presence.
//! A record is read away from the loop ([`crate::platform::watched`]) and a change asks the desk for
//! the frame that lists it; what this device has seen of a workspace is its own ([`HistoryMaps`]) and
//! never reaches a shared record.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

// The page runs this too, where std's clock does not exist: time is read
// through the toolkit's.
use ui::time::Instant;

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

use crate::activity_provider::ActivityWire;
use crate::desk_doc::{DeskDoc, Kept};
use crate::platform::Store;
use crate::platform::apps::Apps;
use crate::platform::authors::{People, Writers};
use crate::platform::pane::Pane;
#[cfg(target_os = "linux")]
#[cfg(target_os = "linux")]
use tilo_actions::remote::workspace_activity as wire;
use ui::record::{RecordCheckout, RecordId, RecordKind};

#[cfg(target_os = "linux")]
use crate::platform::fabric::SharedLink;
#[cfg(target_os = "linux")]
use crate::unlock_history::UnlockHistory;

/// The file this device's view was kept in before it was a record.
const VIEW_FILE: &str = "activity-history.json";

/// The package this device's view of the activity is kept under: the session's.
const VIEW_PACKAGE: &str = "so.tilo.session.activity";

/// Seconds since the epoch, as a change of the desk names its time.
pub(crate) fn now_seconds() -> i64 {
    ui::time::SystemTime::now()
        .duration_since(ui::time::UNIX_EPOCH)
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
    /// The one stream of record changes this host feeds.
    pub changes: crate::platform::records::Changes,
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
    /// The records each workspace holds, read on a worker of its own, told by the change stream
    /// when one of them moved.
    records:
        RefCell<HashMap<String, Rc<crate::platform::records::Reading<Vec<DetachedRecordHistory>>>>>,
    store: Option<Store>,
    /// The one stream of record changes: what tells this session a workspace's records moved.
    changes: crate::platform::records::Changes,
    /// The stream's count and the reads that landed, as the last frame saw them: a change the
    /// person has not been shown asks for the frame that shows it.
    seen: Cell<(u64, u64)>,
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
        changes: crate::platform::records::Changes,
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
            changes,
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
            changes,
            wake,
        } = given;
        let woken = Arc::new(AtomicBool::new(false));
        let requests = activity_requests(Arc::clone(&woken));
        let seen = (changes.moved(), 0);
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
            records: RefCell::default(),
            store,
            changes,
            seen: Cell::new(seen),
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
            notifications.tell(&crate::notices::refused_write_notice(
                &label,
                refusal.reason,
            ));
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

    /// Whether a window asked, or a record moved and was read again, since last taken.
    pub fn take_woken(&self) -> bool {
        let now = (self.0.changes.moved(), self.reads());
        let moved = self.0.seen.replace(now) != now;
        self.0.woken.swap(false, Ordering::AcqRel) || moved
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

    /// The records `workspace_id` holds: read when the change stream says one of them moved, on a
    /// worker where this host has threads, so a workspace's records never hold up a frame.
    fn records_of(&self, workspace_id: WorkspaceId) -> Vec<DetachedRecordHistory> {
        let Some(store) = self.0.store.clone() else {
            return Vec::new();
        };
        let key = workspace_id.to_string();
        let watch = crate::platform::records::Watch::Workspace(key.clone());
        if let Some(known) = self.0.records.borrow().get(&key) {
            let known = Rc::clone(known);
            return known.following(&self.0.changes, &watch).as_ref().clone();
        }
        let reading = store.clone();
        let by_workspace = key.clone();
        let noted = crate::platform::authors::new_noted({
            let noted = self.0.writers.noted.clone();
            move |actor| noted(actor)
        });
        let read = move || read_workspace_records(reading.as_ref(), &by_workspace, &noted);
        #[cfg(target_os = "linux")]
        let known = Rc::new(crate::platform::records::Reading::start(
            "tilo-activity-records",
            self.0.changes.clone(),
            crate::platform::records::Watch::Workspace(key.clone()),
            read,
            {
                let woken = Arc::clone(&self.0.woken);
                let wake = self.0.wake.clone();
                move || ask_for_frame(&woken, wake.as_ref())
            },
        ));
        #[cfg(not(target_os = "linux"))]
        let known = Rc::new(crate::platform::records::Reading::read(
            &self.0.changes,
            &watch,
            read,
        ));
        let value = known.get().as_ref().clone();
        self.0.records.borrow_mut().insert(key, Rc::clone(&known));
        value
    }

    /// The reads of the workspaces' records that landed: their count tells a frame that what the
    /// history reads changed since it was last served.
    fn reads(&self) -> u64 {
        self.0
            .records
            .borrow()
            .values()
            .map(|known| known.reads())
            .sum()
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
        // What the records say moved, and the reads of them that landed since: a history open on
        // this device is served again.
        self.0.changes.moved().hash(&mut hasher);
        self.reads().hash(&mut hasher);
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

/// Every record a workspace holds, with the document its app last wrote.
fn read_workspace_records(
    store: &dyn DurableStore,
    workspace: &str,
    noted: &crate::platform::authors::Noted,
) -> Vec<DetachedRecordHistory> {
    let mut read = Vec::new();
    let mut seen = BTreeSet::new();
    for (package, index_room) in workspace_packages(store, workspace) {
        let Some((checkouts, primary)) =
            crate::platform::records::workspace_records(store, &package, &index_room)
        else {
            continue;
        };
        // The app's own record (Notes' notebook, Chat's chat) is the workspace's document itself,
        // which also keeps the index: read as a record of its own once anything was done in it.
        if let Some(document) = primary
            && seen.insert((
                package.clone(),
                RecordId::package_primary().as_str().to_owned(),
            ))
        {
            noted(document.get_actor());
            read.push(DetachedRecordHistory {
                app: package.clone(),
                checkout: RecordCheckout::package_primary(RecordKind::new(package.as_str())),
                document: Some(document),
            });
        }
        for checkout in checkouts {
            let id = checkout.id.as_str().to_owned();
            if !seen.insert((package.clone(), id.clone())) {
                continue;
            }
            let document = crate::platform::records::document(store, &package, workspace, &id);
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

/// The most bytes of the line a device says of what its person is doing: what a presence status
/// carries.
const MAX_DOING_BYTES: usize = collab_types::presence::MAX_ACTIVITY_BYTES;

/// What someone is doing now, read off the newest of their own steps (`actions`, oldest first, as
/// [`Activity::own_actions`] gives them): "Writing in #general", "Editing ‘Launch plan’ in Notes".
/// The verb comes from the step's title (an app's past-tense history sentence, "Posted a message",
/// "Edited the text") where it says plainly what kind of work it was, else it is "Working on". A
/// record named like its app (a chat's one record, "Chat") is said once: "Working in Chat", never
/// "Working on Chat in Chat". `None` while they took no step lately.
#[must_use]
pub fn doing_now(actions: &[OwnAction]) -> Option<String> {
    let action = actions.last()?;
    let title = action.action.trim();
    let verb = doing(title);
    let resource = action.resource.trim();
    let app = action.app.trim();
    let line = if resource.is_empty() && app.is_empty() {
        title.to_owned()
    } else if resource.is_empty() || resource.eq_ignore_ascii_case(app) || resource.starts_with('#')
    {
        // In the app as a whole, or in a channel, named as the app shows it.
        let place = if resource.starts_with('#') {
            resource
        } else {
            app
        };
        format!("{} in {place}", verb.unwrap_or("Working"))
    } else {
        let verb = verb.unwrap_or("Working on");
        if app.is_empty() {
            format!("{verb} ‘{resource}’")
        } else {
            format!("{verb} ‘{resource}’ in {app}")
        }
    };
    let line = clipped(&line, ui::MAX_STATUS_CHARS, MAX_DOING_BYTES);
    (!line.is_empty()).then_some(line)
}

/// The present-tense verb for a step's title, where its first word says what kind of work it was:
/// writing, editing, drawing, reviewing.
fn doing(title: &str) -> Option<&'static str> {
    let first = title
        .split_whitespace()
        .next()?
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase();
    Some(match first.as_str() {
        "post" | "posted" | "send" | "sent" | "reply" | "replied" | "write" | "wrote"
        | "answer" | "answered" | "message" | "messaged" => "Writing",
        "edit" | "edited" | "change" | "changed" | "type" | "typed" | "rename" | "renamed"
        | "format" | "formatted" | "rewrite" | "rewrote" => "Editing",
        "draw" | "drew" | "sketch" | "sketched" | "paint" | "painted" => "Drawing",
        "comment" | "commented" | "review" | "reviewed" | "resolve" | "resolved" | "suggest"
        | "suggested" => "Reviewing",
        "add" | "added" | "create" | "created" | "make" | "made" | "insert" | "inserted" => {
            "Adding to"
        }
        _ => return None,
    })
}

/// At most `chars` characters and `bytes` bytes of `text`, an ellipsis marking a cut.
fn clipped(text: &str, chars: usize, bytes: usize) -> String {
    if text.chars().count() <= chars && text.len() <= bytes {
        return text.to_owned();
    }
    let mut kept = String::new();
    for (index, c) in text.chars().enumerate() {
        if index.saturating_add(1) >= chars
            || kept
                .len()
                .saturating_add(c.len_utf8())
                .saturating_add('…'.len_utf8())
                > bytes
        {
            break;
        }
        kept.push(c);
    }
    let mut kept = kept.trim_end().to_owned();
    kept.push('…');
    kept
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

    fn step(action: &str, resource: &str, app: &str) -> super::OwnAction {
        super::OwnAction {
            id: String::new(),
            action: action.into(),
            resource: resource.into(),
            app: app.into(),
            at_ms: 0,
        }
    }

    #[test]
    fn the_line_says_what_kind_of_work_and_where_from_the_newest_step() {
        use super::doing_now;
        assert_eq!(doing_now(&[]), None, "no step lately, nothing to say");
        let older = step("Opened", "Plan", "Notes");
        assert_eq!(
            doing_now(&[older.clone(), step("Posted a message", "#general", "Chat")]).as_deref(),
            Some("Writing in #general")
        );
        assert_eq!(
            doing_now(&[step("Post", "Chat", "Chat")]).as_deref(),
            Some("Writing in Chat"),
            "a bare verb reads the same"
        );
        assert_eq!(
            doing_now(&[step("Opened", "chat", "Chat")]).as_deref(),
            Some("Working in Chat"),
            "a record named like its app is said once, whatever the case"
        );
        assert_eq!(
            doing_now(&[step("Edited the text", "Launch plan", "Notes")]).as_deref(),
            Some("Editing ‘Launch plan’ in Notes")
        );
        assert_eq!(
            doing_now(&[older]).as_deref(),
            Some("Working on ‘Plan’ in Notes")
        );
        let long = doing_now(&[step("Edited", &"é".repeat(200), "Notes")]).expect("a line");
        assert!(
            long.len() <= collab_types::presence::MAX_ACTIVITY_BYTES,
            "{long}"
        );
        assert!(long.ends_with('…'));
    }

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
            let activity = Activity::new(
                None,
                None,
                "ada".to_string(),
                None,
                crate::platform::records::Changes::default(),
                None,
                None,
            );
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
            ui::Widget::update(
                &mut self.surface,
                &ui::automerge::AutoCommit::new(),
                &ui::automerge::ROOT,
            );
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

    /// Lines an agent appends to `body` of `notebook` as one run, each a change of its own a second
    /// after the last.
    #[cfg(target_os = "linux")]
    fn agent_appends(
        notebook: &mut ui::automerge::AutoCommit,
        body: &ui::automerge::ObjId,
        time: &mut i64,
        lines: usize,
    ) {
        use ui::automerge::ReadDoc as _;
        use ui::automerge::transaction::Transactable as _;
        for line in 0..lines {
            *time = time.saturating_add(1);
            let wrote = tilo_actions::ActionContext {
                invoker: Some(
                    tilo_actions::ActionInvoker::actor("agent:Claude Code".to_owned())
                        .with_kind(tilo_actions::ActorKind::Agent)
                        .acting_for("ada".to_owned()),
                ),
                what: Some(tilo_actions::Did::Verb(tilo_actions::VerbCall::new(
                    tilo_actions::ActionKey::new(
                        "so.tilo.notes",
                        "notes.notebook",
                        "append-paragraph",
                    ),
                    std::iter::empty(),
                ))),
                ..tilo_actions::ActionContext::default()
            }
            .with_group("run-7", "Claude Code", "agent-run");
            let end = notebook.length(body);
            notebook
                .splice_text(body, end, 0, &format!("Launch notes, line {line}.\n"))
                .unwrap();
            notebook.commit_with(
                ui::automerge::transaction::CommitOptions::default()
                    .with_message(wrote.change_message().expect("a message"))
                    .with_time(*time),
            );
        }
    }

    /// An agent's work in an app's own record (Notes' notebook is the workspace's Notes document,
    /// which also keeps its index) is one of its runs, with what it did: the record a host opens
    /// for an agent is that one. Its card lists the run, and the notice that it finished counts
    /// the record it changed. Each time it writes again, the history reads what it added and
    /// lists that too.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_agents_run_in_an_apps_own_record_is_listed_with_its_changes() {
        use ui::automerge::transaction::Transactable as _;
        let dir = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(app_state::FsStore::new(dir.path())) as Store;
        let mut desk = Desk::new();
        let workspace = desk
            .handle
            .read(|tree| tree.active_workspace().map(Workspace::id))
            .expect("a workspace")
            .to_string();
        // The notebook as a host keeps it: what the agent wrote, for the person, in its run.
        let mut notebook = ui::automerge::AutoCommit::new();
        let body = notebook
            .put_object(ui::automerge::ROOT, "body", ui::automerge::ObjType::Text)
            .unwrap();
        notebook.commit();
        let key = StorageKey::workspace_index("so.tilo.notes", &workspace);
        let _lease = store.lease(&key).unwrap();
        // Each write is noted on the one stream of record changes, as the store's writers note it.
        let changes = crate::platform::records::Changes::default();
        let mut time = 0;
        let mut write = |notebook: &mut ui::automerge::AutoCommit, lines: usize| {
            agent_appends(notebook, &body, &mut time, lines);
            store
                .write(&key, WriteOp::Replace(notebook.save()), None)
                .unwrap();
            changes.note(&workspace, "so.tilo.notes", "", false);
        };
        write(&mut notebook, 2);

        desk.activity = Activity::over(Given {
            desk: DeskDoc::load(None, None, "ada"),
            person: "ada".into(),
            store: Some(Arc::clone(&store)),
            state: None,
            apps: None,
            people: crate::platform::authors::unnamed(),
            writers: Writers::untracked(),
            pane: Rc::new(crate::platform::pane::Linked),
            models: None,
            changes: changes.clone(),
            wake: None,
        });
        desk.surface
            .set_history_source(Box::new(desk.activity.clone()));
        desk.surface.serve_activity(desk.activity.requests());
        desk.open_history();
        let invocation = InvocationId::new();
        let InvokeArgument::Json(query) = wire::argument(&wire::RunsQuery::default()) else {
            panic!("the runs take a query");
        };
        desk.activity
            .requests()
            .follow(&person(), invocation, wire::RUNS, query)
            .expect("the runs are followed");
        // The records are read again on a worker once the stream says they moved: a pass lists
        // what that read found once it landed.
        let mut runs_until = |landed: &dyn Fn(&wire::AgentRun) -> bool| -> wire::Runs {
            for _ in 0..300 {
                desk.frame();
                let runs: wire::Runs = serde_json::from_value(
                    desk.activity
                        .requests()
                        .served(&invocation)
                        .expect("the runs are served")
                        .expect("runs"),
                )
                .expect("runs");
                if runs.runs.iter().any(landed) {
                    return runs;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("the agent's run is never listed");
        };
        let first = runs_until(&|run| run.last_ms == 2_000);
        let [run] = first.runs.as_slice() else {
            panic!("the agent's one run: {first:?}");
        };
        assert_eq!(run.agent.name, "Claude Code", "{run:?}");
        let [action] = run.actions.as_slice() else {
            panic!("one action, in the notebook: {run:?}");
        };
        assert_eq!(action.app_id, "so.tilo.notes", "{action:?}");
        assert_eq!(
            action.record,
            ui::record::RecordId::package_primary().as_str(),
            "{action:?}"
        );
        let finished = tilo_desktop::here::AgentFinished {
            publisher: String::new(),
            label: "Claude Code".into(),
            last: None,
        };
        assert_eq!(
            finished.summary(&first).map(|summary| summary.changes),
            Some(1),
            "the finished notice counts the notebook it changed"
        );

        // It writes again: the run ends at what it added, its fifth change.
        write(&mut notebook, 3);
        let again = runs_until(&|run| run.last_ms == 5_000);
        assert_eq!(again.runs.len(), 1, "still the agent's one run: {again:?}");
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
            feed.entries[0].author.name, "ada",
            "the row carries the account's name, what its face and the search read"
        );
        assert_eq!(
            ui::wire_author_line(&feed.entries[0].author),
            "You",
            "and the line words the person reading it as such, never by their account's name"
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
