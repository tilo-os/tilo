//! The workspace's activity, read from what this device keeps of it: the
//! desk's own document ([`crate::desk_doc`]), and the records the workspace's
//! apps keep in the store.
//!
//! The desk is one automerge document, and every change of it carries who made
//! it and when. The activity pane reads those changes directly
//! ([`ActivitySnapshot::of_desktop`]): the windows opened and closed, the
//! strips renamed, the layouts restored, and who did each. A record the
//! workspace holds carries its own history in the store, read here as a
//! detached copy of the document its app writes.
//!
//! What the workspace holds is read away from the loop ([`Watched`]): the
//! records belong to apps this process does not own, and one of them being
//! slow to read must not hold up a frame. A change of what the history reads
//! — a record an app wrote, an answer another device made — asks the desk
//! for one frame, which is how an open history lists it with nobody asking.
//! What this device has seen of a workspace is its own, kept in one file
//! ([`HistoryMaps`]): only the person here marks it, and it never reaches a
//! shared record.
//!
//! The desk's layout is this session's to write, so a change the person
//! undoes or restores reaches the desk itself: the model hands the workspace
//! back ([`ActivitySnapshot::committing`]), and the session takes it up as
//! its own change on the pass after the frame it was made in.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use automerge::{AutoCommit, LoadOptions, OnPartialLoad};
use durable_store::{DurableStore, StorageKey};
use serde_json::Value;
use tilo_desktop_model::{DesktopTree, Workspace, WorkspaceId, WorkspaceSlot};
use tilo_desktop_surface::HistorySource;
use tilo_desktop_surface::activity::describer::{Describer, Job};
use tilo_desktop_surface::activity::{
    ActivityHost, ActivitySnapshot, DetachedRecordHistory, HistoryOrigin, WorkspaceSeenFrontier,
    morning,
};
use tilo_desktop_surface::history_log;
use tilo_desktop_surface::session::{ActivityRequests, activity_requests};
use tilo_device_fabric::link::SharedLink;
use tilo_files::native::Watched;
use tilo_lock::MutexRecover as _;
use ui::record::RecordCheckout;

use crate::activity_provider::ActivityWire;
use crate::desk_doc::{DeskDoc, Kept};
use crate::lists::Lists;
use crate::unlock_history::UnlockHistory;
use tilo_actions::remote::workspace_activity as wire;

/// What this device keeps of its own view of the workspaces' activity: the
/// changes it has seen, when it left a workspace, and the morning sources it
/// chose.
const VIEW_FILE: &str = "activity-history.json";

/// Seconds since the epoch, as a change of the desk names the time it was
/// made.
pub(crate) fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
        })
}

/// What the person has been told about one device's lasting refusals: the
/// workspace it refuses in, the device, and the class of its reason. A device
/// that refuses a person's next change for the same reason adds another row
/// to the history, not another notification.
type ToldRefusal = (String, [u8; 32], neighbour_wire::RefusalReason);

/// A workspace's activity, as this session serves it: to the desk's own
/// model, and to the apps that ask through the broker.
#[derive(Clone)]
pub(crate) struct Activity(Rc<Inner>);

struct Inner {
    /// The desk's document, whose changes are its history, and where it is
    /// kept.
    desk: DeskDoc,
    /// What the person's devices answered when one of them asked to be
    /// unlocked, as their own record.
    unlock: UnlockHistory,
    /// Who sits here.
    person: String,
    host: Rc<NativeActivityHost>,
    /// The records each workspace holds, read on a worker of its own.
    records: RefCell<HashMap<String, Rc<Watched<Vec<DetachedRecordHistory>>>>>,
    /// Where the records are read from; `None` on a session that keeps no
    /// records store.
    store: Option<PathBuf>,
    /// Moves whenever the records read for a workspace moved.
    records_moved: Arc<AtomicU64>,
    /// The workspace a change the model wrote back leaves the desk at, for
    /// the session to take up on its next pass.
    restored: RefCell<Option<DesktopTree>>,
    /// The resources the person opened from the history, for the session to
    /// open their app on.
    openings: RefCell<Vec<(WorkspaceId, ui::HistoryResourceTarget)>>,
    /// The windows' end of what they ask of the activity, and what they
    /// follow.
    requests: ActivityRequests,
    /// Raised when a window asked or followed something, or when what the
    /// history reads moved: the desk draws a frame to take it.
    woken: Arc<AtomicBool>,
    /// The broker's end of the same activity, once this session serves the
    /// apps it runs.
    wire: RefCell<Option<Arc<ActivityWire>>>,
    /// The revision the model last served its reads at, as the apps
    /// following them were last told.
    served: Cell<Option<u64>>,
    /// What this device has already told the person about its lasting
    /// refusals: one notice per device and reason, never one per write or per
    /// pass.
    told_refusals: RefCell<BTreeSet<ToldRefusal>>,
    /// What the desk's history says its changes were, as a model answered it.
    describer: RefCell<Describer>,
    /// Answers the model gave, waiting for the desk's thread to write them.
    described: Arc<Mutex<Vec<(Job, String)>>>,
    /// Whether a model is being asked right now: one request at a time keeps
    /// a long history from spending the person's provider on a burst.
    describing: Arc<AtomicBool>,
    /// The desk's changes the describer last read, so a pass with nothing new
    /// asks nothing.
    described_heads: RefCell<Vec<automerge::ChangeHash>>,
    /// When a model was last looked for: a provider the person configures
    /// while the session runs is taken up without a frame per lookup.
    looked_for_model: Cell<Option<Instant>>,
    /// What wakes the session's loop when a model answered off this thread.
    wake: Option<tilo_service::Wake>,
}

/// How often a session with nothing new on its desk looks again for a model
/// the person may have configured meanwhile.
const LOOK_FOR_MODEL: Duration = Duration::from_secs(30);

/// Something the history reads moved — a record an app wrote, an answer
/// another of the person's devices made: the desk draws a frame to read it,
/// as it does for a window that asked, and the session's loop wakes to serve
/// it. Raised by a change alone, never by a pass or a clock.
fn ask_for_frame(woken: &AtomicBool, wake: Option<&tilo_service::Wake>) {
    woken.store(true, Ordering::Release);
    if let Some(wake) = wake {
        wake();
    }
}

impl Activity {
    /// This session's activity: the desk's document under `state` (kept in
    /// memory alone without one), the desk an older Tilo kept at `old_desk`
    /// brought over when this device has none, the records under `store`, and
    /// the person here.
    pub(crate) fn new(
        state: Option<&Path>,
        old_desk: Option<&Path>,
        person: String,
        store: Option<PathBuf>,
        lists: Lists,
        wake: Option<tilo_service::Wake>,
    ) -> Self {
        let woken = Arc::new(AtomicBool::new(false));
        let requests = activity_requests(Arc::clone(&woken));
        Self(Rc::new(Inner {
            desk: DeskDoc::load(state, old_desk, &person),
            unlock: UnlockHistory::new(state, &person),
            host: Rc::new(NativeActivityHost {
                person: person.clone(),
                maps: HistoryMaps::load(state),
                lists,
                engine: RefCell::default(),
            }),
            person,
            records: RefCell::default(),
            store,
            records_moved: Arc::default(),
            restored: RefCell::default(),
            openings: RefCell::default(),
            requests,
            woken,
            wire: RefCell::default(),
            served: Cell::default(),
            told_refusals: RefCell::default(),
            describer: RefCell::default(),
            described: Arc::default(),
            describing: Arc::default(),
            described_heads: RefCell::default(),
            looked_for_model: Cell::default(),
            wake,
        }))
    }

    /// Show where a workspace's writes were refused from `engine`, the
    /// workspaces this device holds: the history rows read the verdicts this
    /// device recorded, and the one place the person hears of a lasting one.
    fn attach_engine(&self, engine: Arc<crate::engine::Engine>) {
        *self.0.host.engine.borrow_mut() = Some(engine);
    }

    /// Tell the person about the refusals of this device's writes that waiting
    /// cannot change, once each: a device that will not take the write says
    /// why, because the change is not arriving there. A verdict that is gone
    /// is forgotten, so a device that refuses again later is heard again.
    fn tell_refusals(&self, notifications: &crate::notifications::SessionNotifications) {
        let Some(engine) = self.0.host.engine.borrow().as_ref().map(Arc::clone) else {
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

    /// Open this device's workspaces and show them in the activity: what
    /// refused one of the history's writes, and what the devices call
    /// themselves.
    pub(crate) fn open_engine(
        &self,
        options: &crate::Options,
        handle: &crate::DesktopHandle,
    ) -> Option<Arc<crate::engine::Engine>> {
        let engine = crate::options::open_engine(options, handle)?;
        self.attach_engine(Arc::clone(&engine));
        Some(engine)
    }

    /// The desk this device left, as its document holds it: what the session
    /// starts from, and what became of the desk on disk when this build did
    /// not open it.
    pub(crate) fn opened_desk(&self) -> Kept {
        self.0.desk.opened()
    }

    /// What the windows ask of the activity and follow.
    pub(crate) fn requests(&self) -> ActivityRequests {
        self.0.requests.clone()
    }

    /// Serve the apps this session runs: `wire` reads what the model served,
    /// and is told when it served something new.
    pub(crate) fn serve_apps(&self, wire: Arc<ActivityWire>) {
        *self.0.wire.borrow_mut() = Some(wire);
    }

    /// Take part in `link`'s fabric: what the person's devices answer each
    /// other about unlocking a disk is kept in their own record here.
    pub(crate) fn watch_fabric(&self, link: SharedLink) {
        self.0.unlock.watch(link);
    }

    /// Write what the person's fabric holds now into their own record,
    /// whether or not a window shows the history: an answer stands for
    /// minutes, so it is written while it is there to be read. An answer the
    /// record did not hold yet is a change of the history, and asks the desk
    /// for the frame that lists it.
    pub(crate) fn keep_unlock(&self) {
        if self.0.unlock.absorb() {
            ask_for_frame(&self.0.woken, self.0.wake.as_ref());
        }
    }

    /// Serve what the activity holds on this pass: tell the apps following it
    /// that what they read may have moved — once the model served its reads at
    /// a revision they have not been told of — and tell the person about the
    /// refusals of this device's writes that will not change on their own.
    pub(crate) fn serve(&self, notifications: &crate::notifications::SessionNotifications) {
        let served = self.0.requests.served_revision();
        if served != self.0.served.get() {
            self.0.served.set(served);
            if let Some(wire) = self.0.wire.borrow().as_ref() {
                wire.changed();
            }
        }
        self.tell_refusals(notifications);
    }

    /// Record the desk as `tree` holds it, at `time` (seconds since the
    /// epoch), by the person here: one change of the desk's document, and the
    /// document kept. Whether the desk changed at all.
    pub(crate) fn record_desk(
        &self,
        tree: &DesktopTree,
        time: i64,
        shows: &[(tilo_desktop_model::TabId, String)],
    ) -> bool {
        self.0.desk.record(tree, time, shows)
    }

    /// [`Self::record_desk`], and write even how the desk looks now: what ends
    /// the session, which the next one starts from.
    pub(crate) fn record_desk_now(
        &self,
        tree: &DesktopTree,
        time: i64,
        shows: &[(tilo_desktop_model::TabId, String)],
    ) -> bool {
        self.0.desk.record_now(tree, time, shows)
    }

    /// Record `tree` as `asked`'s arranging action: one change of the desk
    /// whose own entry names who asked and what they did. Whether the desk
    /// changed at all.
    pub(crate) fn record_action(
        &self,
        tree: &DesktopTree,
        time: i64,
        asked: &tilo_actions::ActionContext,
        shows: &[(tilo_desktop_model::TabId, String)],
    ) -> bool {
        self.0.desk.record_action(tree, time, asked, shows)
    }

    /// The record each window of the desk shows, by tab, as the desk keeps
    /// it.
    pub(crate) fn records(&self) -> Vec<(tilo_desktop_model::TabId, String)> {
        self.0.desk.records()
    }

    /// Take back `person`'s last step of the desk, and put the desk it leaves
    /// where the session takes it up ([`Self::take_restored`]). Whether there
    /// was a step.
    pub(crate) fn undo_desk(&self, person: &str) -> bool {
        self.rewind(person, false)
    }

    /// Bring back the last step of the desk `person` took back.
    pub(crate) fn redo_desk(&self, person: &str) -> bool {
        self.rewind(person, true)
    }

    fn rewind(&self, person: &str, redo: bool) -> bool {
        let stepped = if redo {
            self.0.desk.redo(person)
        } else {
            self.0.desk.undo(person)
        };
        if stepped && let Some(tree) = self.0.desk.tree() {
            *self.0.restored.borrow_mut() = Some(tree);
        }
        stepped
    }

    /// The last step of the desk's history, as who made it and what it did.
    pub(crate) fn last_step(&self, person: &str) -> Option<(String, String)> {
        self.0.desk.last_step(person)
    }

    /// The workspace a change the model wrote back leaves the desk at, if
    /// one is waiting.
    pub(crate) fn take_restored(&self) -> Option<DesktopTree> {
        self.0.restored.borrow_mut().take()
    }

    /// The resources the person opened from the history since this was last
    /// taken, each with the workspace it was opened in.
    pub(crate) fn take_openings(&self) -> Vec<(WorkspaceId, ui::HistoryResourceTarget)> {
        std::mem::take(&mut *self.0.openings.borrow_mut())
    }

    /// Whether a window asked the model something, or what the history reads
    /// moved, since this was last taken: the desk draws a frame to take it.
    pub(crate) fn take_woken(&self) -> bool {
        self.0.woken.swap(false, Ordering::AcqRel)
    }

    /// When this session next looks for a model, so a provider the person
    /// configures while it runs is taken up with no pass in between.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.0
            .looked_for_model
            .get()
            .and_then(|at| at.checked_add(LOOK_FOR_MODEL))
    }

    /// Say what the desk's changes were, and name the runs of them, when the
    /// desk moved since they were last read: one request at a time, off the
    /// desk's thread, written into the desk's document when the answer comes
    /// so every member reads the same words. Whether the document changed and
    /// the desk has something new to show.
    pub(crate) fn describe(&self) -> bool {
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
        describer.set_enabled(crate::activity_models::configured());
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
        std::thread::spawn(move || {
            let answer = crate::activity_models::ask(&job).unwrap_or_default();
            answers.lock_recover().push((job, answer));
            describing.store(false, Ordering::Release);
            // The desk draws a frame to write the answer where it belongs.
            woken.store(true, Ordering::Release);
            if let Some(wake) = &wake {
                wake();
            }
        });
        wrote
    }

    /// The `WorkspaceId` of `workspace`, as `tree` holds it.
    fn id_of(tree: &DesktopTree, workspace: WorkspaceSlot) -> Option<WorkspaceId> {
        tree.workspaces
            .iter()
            .find(|held| held.slot == workspace)
            .map(Workspace::id)
    }

    /// The records the workspace `workspace_id` holds, as the worker last
    /// read them: made when the history is first read for the workspace, and
    /// following them from then on.
    fn records_of(
        &self,
        workspace_id: WorkspaceId,
    ) -> Option<Rc<Watched<Vec<DetachedRecordHistory>>>> {
        let store = self.0.store.clone()?;
        let key = workspace_id.to_string();
        if let Some(known) = self.0.records.borrow().get(&key) {
            return Some(Rc::clone(known));
        }
        let directories = record_directories(&store, &key);
        let by_workspace = key.clone();
        let read = move || read_workspace_records(&store, &by_workspace);
        let watch = Watched::start("tilo-activity-records", directories, read).ok()?;
        let moved = Arc::clone(&self.0.records_moved);
        // The record was read again because it changed: the model reading it
        // is told, so an open history lists the change with no one asking.
        let woken = Arc::clone(&self.0.woken);
        let wake = self.0.wake.clone();
        watch.on_change(move || {
            moved.fetch_add(1, Ordering::AcqRel);
            ask_for_frame(&woken, wake.as_ref());
        });
        let watch = Rc::new(watch);
        self.0.records.borrow_mut().insert(key, Rc::clone(&watch));
        Some(watch)
    }
}

impl HistorySource for Activity {
    fn workspace_history(
        &mut self,
        tree: &DesktopTree,
        workspace: WorkspaceSlot,
    ) -> Option<ActivitySnapshot> {
        Self::id_of(tree, workspace)?;
        // The desk as it stands is the newest change of it: a workspace made
        // this pass is in the document the pane reads, not only in the tree.
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
        if let Some(workspace_id) = Self::id_of(tree, workspace)
            && let Some(watch) = self.records_of(workspace_id)
        {
            let held = tree.workspace_records_held.get(&workspace_id);
            let read = watch.get();
            records.extend(
                read.iter()
                    .filter(|record| {
                        held.is_none_or(|held| held.contains_key(record.checkout.id.as_str()))
                    })
                    .cloned(),
            );
        }
        // What the person's devices answered when one of them asked to be
        // unlocked is theirs and not any one workspace's: it reads the same
        // in every workspace's history.
        if let Some(record) = self.0.unlock.history() {
            records.push(record);
        }
        records
    }

    fn host(&self) -> Rc<dyn ActivityHost> {
        Rc::clone(&self.0.host) as Rc<dyn ActivityHost>
    }

    fn register_authors(&self, authors: &mut ui::AuthorDirectory) {
        // The names the desk's changes are known by: the person here, and
        // whoever the desk came from when it was read into its document.
        let known = self.0.desk.authors();
        history_log::register_authors(authors, &known, &|_| None);
        // The changes this device made are the person's own: they read as
        // presence shows them here, not as another participant.
        let mine = self.0.person.clone();
        for (actor, author) in &known {
            if *author == mine {
                authors.register_local_identity(actor);
            }
        }
        // The devices that answered a boot are named as their own snapshots
        // name them, and this one of them is the person here.
        history_log::register_authors(authors, &self.0.unlock.authors(), &|_| None);
        if let Some(actor) = self.0.unlock.local_actor() {
            authors.register_local_identity(&actor);
        }
    }

    fn revision(&self) -> u64 {
        use std::hash::{Hash as _, Hasher as _};
        // What the fabric holds now is written before the record is read: a
        // pass that hears an answer is the pass that lists it. The desk is
        // reading the history as it stands, so this write needs no frame.
        self.0.unlock.absorb();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.0.desk.heads().hash(&mut hasher);
        self.0.unlock.heads().hash(&mut hasher);
        self.0
            .records_moved
            .load(Ordering::Acquire)
            .hash(&mut hasher);
        hasher.finish()
    }

    /// The person opened a resource from the history: its app shows it. No
    /// window here is bound to a record, so the session opens the app on the
    /// target on its next pass, after the frame that asked.
    fn resource_tab(
        &self,
        tree: &DesktopTree,
        target: &ui::HistoryResourceTarget,
    ) -> Option<tilo_desktop_model::TabId> {
        // No app holds the person's own record of what their devices
        // answered: its rows are read in the history, and opened nowhere.
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

/// The directories that change when a record a workspace holds does: the
/// workspace's index, where its app names the records it holds, and each
/// record's, where the app writes the document. A record an app starts
/// holding after this is read is picked up by its workspace's index; what the
/// app writes into it after that is read when a session reads the history
/// again.
fn record_directories(store: &Path, workspace: &str) -> Vec<PathBuf> {
    let device: Rc<dyn DurableStore> = Rc::new(app_state::FsStore::new(store));
    let indexes = match device.workspace_indexes() {
        Ok(indexes) => indexes,
        Err(error) => {
            tracing::warn!(%error, "the workspace indexes of this device are not listed");
            return Vec::new();
        }
    };
    let mut directories = BTreeSet::new();
    for (package, index_room) in indexes {
        if index_room != workspace {
            continue;
        }
        let Ok(directory) = app_state::workspace_index_directory(store, &package, &index_room)
        else {
            continue;
        };
        for record in named_in(device.as_ref(), &package, &index_room) {
            directories.insert(directory.join("records").join(record.id.as_str()));
        }
        directories.insert(directory);
    }
    directories.into_iter().collect()
}

/// Every record a workspace holds, as its apps' indexes name them, with the
/// document its app last wrote. One the index does not name is one no app of
/// the workspace may open, so it is not read here.
fn read_workspace_records(store: &Path, workspace: &str) -> Vec<DetachedRecordHistory> {
    let device: Rc<dyn DurableStore> = Rc::new(app_state::FsStore::new(store));
    let indexes = match device.workspace_indexes() {
        Ok(indexes) => indexes,
        Err(error) => {
            tracing::warn!(%error, "the workspace indexes of this device are not listed");
            return Vec::new();
        }
    };
    let mut read = Vec::new();
    let mut seen = BTreeSet::new();
    for (package, index_room) in indexes {
        if index_room != workspace {
            continue;
        }
        for checkout in named_in(device.as_ref(), &package, &index_room) {
            let id = checkout.id.as_str().to_owned();
            if !seen.insert((package.clone(), id.clone())) {
                continue;
            }
            let document = document_of(device.as_ref(), &package, workspace, &id);
            read.push(DetachedRecordHistory {
                app: package.clone(),
                checkout,
                document,
            });
        }
    }
    read
}

/// The records `package`'s index for `workspace` names, as the store holds it.
fn named_in(store: &dyn DurableStore, package: &str, workspace: &str) -> Vec<RecordCheckout> {
    let Ok(Some(stored)) = store.read(&StorageKey::workspace_index(package, workspace)) else {
        return Vec::new();
    };
    if stored.bytes.is_empty() {
        return Vec::new();
    }
    // An app keeping the index appends to it, and this read may land on a
    // tail it has not finished writing: the changes before it are the index
    // as of the last write that completed.
    let options = LoadOptions::new().on_partial_load(OnPartialLoad::Ignore);
    match AutoCommit::load_with_options(&stored.bytes, options) {
        Ok(doc) => ui::record::index::discoverable_references(&doc),
        Err(error) => {
            tracing::warn!(%error, package, %workspace, "a workspace's index does not load");
            Vec::new()
        }
    }
}

/// The document `package` keeps for record `record` in `workspace`, as its
/// app last committed it: `None` while the app holds changes it has not
/// committed, so the history shows none of them.
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

/// What the model may ask of this session: who is looking, what they have
/// seen, and the names of the apps that hold the records.
struct NativeActivityHost {
    person: String,
    maps: HistoryMaps,
    lists: Lists,
    /// The workspaces this device holds, once the session opened them: what
    /// refused one of the history's writes, and what the devices call
    /// themselves.
    engine: RefCell<Option<Arc<crate::engine::Engine>>>,
}

impl ActivityHost for NativeActivityHost {
    fn local_user(&self) -> String {
        self.person.clone()
    }

    fn local_author(&self, authors: &mut ui::AuthorDirectory, actor: &str) -> Option<String> {
        if authors.register_local_identity(actor) {
            return None;
        }
        (!self.person.is_empty()).then(|| self.person.clone())
    }

    fn load_morning_sources(&self) -> HashMap<u64, morning::MorningPreferences> {
        self.maps.map("morning_sources")
    }

    fn save_morning_sources(&self, sources: &HashMap<u64, morning::MorningPreferences>) {
        self.maps.save("morning_sources", sources);
    }

    fn load_frontiers(&self) -> HashMap<u64, WorkspaceSeenFrontier> {
        self.maps.map("seen_frontiers")
    }

    fn save_frontiers(&self, frontiers: &HashMap<u64, WorkspaceSeenFrontier>) {
        self.maps.save("seen_frontiers", frontiers);
    }

    fn mutations_available(&self) -> bool {
        true
    }

    /// The desk's own layout is this session's to write, so undoing or
    /// restoring a change of it reaches the desk. A record's history is its
    /// app's to write, and this session holds no writer for one.
    fn writable(&self, origin: &HistoryOrigin) -> bool {
        matches!(origin, HistoryOrigin::DesktopTree)
    }

    fn app_display_name(&self, app: &str) -> Option<String> {
        self.lists
            .latest()
            .applications
            .iter()
            .find(|entry| entry.id == app)
            .map(|entry| entry.title.clone())
    }

    /// Each write this device refused in `workspace`, once per change it would
    /// have brought: the history row of that change reads where it is not.
    fn refusals(&self, workspace: &str) -> Vec<wire::Refusal> {
        let Some(engine) = self.engine.borrow().as_ref().map(Arc::clone) else {
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
}

/// This device's private view of the workspaces' activity, kept in one file.
///
/// Read once, when the session starts, and written by the state writer
/// ([`tilo_durable_writer`]).
struct HistoryMaps {
    known: RefCell<serde_json::Map<String, Value>>,
    path: Option<PathBuf>,
}

impl HistoryMaps {
    fn load(state: Option<&Path>) -> Self {
        let path = state.map(|state| state.join("tilo").join(VIEW_FILE));
        let known = path
            .as_deref()
            .and_then(|path| match std::fs::read(path) {
                Ok(bytes) => Some(bytes),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        path = %path.display(),
                        "the remembered history map view could not be read"
                    );
                    None
                }
            })
            .and_then(|bytes| {
                serde_json::from_slice(&bytes)
                    .inspect_err(|error| {
                        tracing::warn!(%error, "the remembered history map view does not read back");
                    })
                    .ok()
            })
            .unwrap_or_default();
        Self {
            known: RefCell::new(known),
            path,
        }
    }

    /// `key`'s per-workspace map, as this session last kept it.
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

    /// Keep `values` as `key`'s map, for this session at once and for the
    /// next one once the burst of saves is over.
    fn save<T: serde::Serialize>(&self, key: &str, values: &HashMap<u64, T>) {
        let map = Value::Object(
            values
                .iter()
                .filter_map(|(id, value)| Some((id.to_string(), serde_json::to_value(value).ok()?)))
                .collect(),
        );
        let mut known = self.known.borrow_mut();
        known.insert(key.to_owned(), map);
        if let Some(path) = &self.path
            && let Ok(bytes) = serde_json::to_vec(&*known)
        {
            tilo_durable_writer::keep(path.clone(), bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    //! A native session's history, as the desk serves it: the activity a
    //! person opens from its command is the workspace's own changes, and
    //! what they restore from it reaches the desk.

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

    /// A desk of one workspace with Notes open, its activity served from
    /// the desk's own log the way this session serves it.
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
            let activity =
                Activity::new(None, None, "ada".to_string(), None, Lists::default(), None);
            surface.set_history_source(Box::new(activity.clone()));
            surface.serve_activity(activity.requests());
            Self {
                surface,
                handle,
                activity,
                workspace,
            }
        }

        /// One pass of the desk: what the windows asked is answered. The desk
        /// as it stood is recorded first, as the pass before this one
        /// recorded it, so what this pass draws is what the desk is.
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

    /// The activity opens from its command as a window of the desk, and it
    /// lists the workspace's real changes: a rename the person made here.
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
        assert_eq!(feed.entries[0].author.name, "ada", "and who did it");
        assert!(
            feed.mutations,
            "the desk's own layout is the person's to undo"
        );
    }

    /// Nothing the history reads moved, so no pass asks the desk for a
    /// frame: the model is read again when a change says so, never on a
    /// clock and never because a frame was drawn.
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

    /// An arrangement action is one step of the desk's own history: the
    /// person takes exactly that step back, and the desk it takes them back
    /// to is staged for the pass to apply.
    #[test]
    fn an_arrangements_action_is_one_step_the_person_takes_back() {
        let mut desk = Desk::new();
        desk.open_history();
        desk.rename("Plans");
        desk.frame();
        assert_eq!(desk.name(), "Plans");

        // An agent moves the desk as the person asked it to: the change it
        // made is recorded as that action, not as a change of the desk's own.
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
        assert!(
            desk.activity
                .record_action(&tree, now_seconds(), &asked, &[]),
            "the desk keeps the move"
        );
        assert_eq!(
            desk.activity.last_step("ada"),
            Some(("agent-task:reading".to_owned(), "move".to_owned())),
            "the step names who asked and what they did"
        );

        // The person takes it back: the move is undone, and the rename before
        // it is still there — the two are separate steps.
        assert!(desk.activity.undo_desk("ada"), "the person takes it back");
        let restored = desk
            .activity
            .take_restored()
            .expect("the desk to take the step back is staged for the pass");
        assert_eq!(
            restored
                .active_workspace()
                .map(|workspace| workspace.name.as_str()),
            Some("Plans"),
            "exactly the move is undone"
        );
        assert!(
            desk.activity.undo_desk("ada"),
            "the rename before it is its own step"
        );
        let restored = desk
            .activity
            .take_restored()
            .expect("and the desk before it is staged too");
        assert_eq!(
            restored
                .active_workspace()
                .map(|workspace| workspace.name.as_str()),
            Some("Studio"),
            "the desk the session started on"
        );
    }

    /// A version restored from the history reaches the desk: the session
    /// takes up what the model wrote and the desk is as it was, and the
    /// restore is itself a change of it.
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
}
