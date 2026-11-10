//! The workspace's activity, read from what this device keeps of it: the desk's own document
//! ([`crate::desk_doc`]), and the records the workspace's apps keep in the store.
//! The desk is one automerge document, and every change of it carries who made it and when. The
//! activity pane reads those changes directly: the windows opened and closed, the strips renamed,
//! the layouts restored, and who did each. A record the workspace holds carries its own history in
//! the store, read here as a detached copy of the document its app writes.
//! What the workspace holds is read away from the loop ([`crate::platform::watched`]): the records
//! belong to apps this process does not own, and one of them being slow to read must not hold up a
//! frame. A change of what the history reads asks the desk for one frame, which is how an open
//! history lists it with nobody asking.
//! What this device has seen of a workspace is its own, kept in one place ([`HistoryMaps`]): only
//! the person here marks it, and it never reaches a shared record.
//! The desk's layout is this session's to write, so a change the person undoes or restores reaches
//! the desk itself: the model hands the workspace back ([`ActivitySnapshot::committing`]), and the
//! session takes it up as its own change on the pass after the frame it was made in.

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
use tilo_desktop_model::{DesktopTree, Workspace, WorkspaceId, WorkspaceSlot};
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

/// The file this device's own view of the workspaces' activity was kept in before it was a
/// record of the store: read once into it, and left where it is.
const VIEW_FILE: &str = "activity-history.json";

/// The package this device's own view of the workspaces' activity is kept under: the session's,
/// where no workspace reaches it.
const VIEW_PACKAGE: &str = "so.tilo.session.activity";

/// Seconds since the epoch, as a change of the desk names the time it was made.
pub(crate) fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
        })
}

/// What the person has been told about one device's lasting refusals: the workspace it refuses in,
/// the device, and the class of its reason. A device that refuses a person's next change for the
/// same reason adds another row to the history, not another notification.
#[cfg(target_os = "linux")]
type ToldRefusal = (String, [u8; 32], neighbour_wire::RefusalReason);

/// What a host gives the workspace's activity: the desk's document, where this device keeps its
/// records, and the seams the history asks it for.
pub struct Given {
    /// The desk's document, as the host keeps it.
    pub desk: DeskDoc,
    /// Who sits here.
    pub person: String,
    /// The store the workspace's records are in, and where this device's own view of them is kept;
    /// `None` on a session that keeps no records store.
    pub store: Option<Store>,
    /// The state directory the previous file of this device's view is read from, once.
    pub state: Option<PathBuf>,
    /// The apps this host has, which name the records they wrote.
    pub apps: Option<Arc<dyn Apps>>,
    /// Who a principal is, as presence shows them.
    pub people: People,
    /// The writers of the records this host's mounts write.
    pub writers: Writers,
    /// Where the activity model's code is.
    pub pane: Rc<dyn Pane>,
    /// The model the desk's changes are described by, while this host answers one.
    pub models: Option<Arc<dyn crate::platform::Models>>,
    /// What moves when the records this device holds moved: read by a host whose store it drives
    /// itself, and `None` where the store is followed by [`Watched`].
    pub stamp: Option<Rc<dyn Fn() -> u64>>,
    /// What wakes the session's loop when a model answered, or a record moved.
    pub wake: Option<tilo_service::Wake>,
}

/// A workspace's activity, as this session serves it: to the desk's own model, and to the apps that
/// ask through the broker.
#[derive(Clone)]
pub struct Activity(Rc<Inner>);

struct Inner {
    /// The desk's document, whose changes are its history, and where it is kept.
    desk: DeskDoc,
    /// What the person's devices answered when one of them asked to be unlocked, as their own
    /// record.
    #[cfg(target_os = "linux")]
    unlock: UnlockHistory,
    /// Who sits here.
    person: String,
    host: Rc<Host>,
    /// The records each workspace holds, read on a worker of its own.
    #[cfg(target_os = "linux")]
    records: RefCell<HashMap<String, Rc<Watched<Vec<DetachedRecordHistory>>>>>,
    /// Where the records are read from; `None` on a session that keeps no records store.
    store: Option<Store>,
    /// Moves whenever the records read for a workspace moved.
    records_moved: Arc<AtomicU64>,
    /// What a host whose store it drives itself answers for "the records moved".
    stamp: Option<Rc<dyn Fn() -> u64>>,
    /// The stamp last read, and the store read it made, while the store is the host's to drive.
    read_at: Cell<Option<(u64, Rc<Vec<DetachedRecordHistory>>)>>,
    /// The workspace a change the model wrote back leaves the desk at, for the session to take up
    /// on its next pass.
    restored: RefCell<Option<DesktopTree>>,
    /// The resources the person opened from the history, for the session to open their app on.
    openings: RefCell<Vec<(WorkspaceId, ui::HistoryResourceTarget)>>,
    /// The windows' end of what they ask of the activity, and what they follow.
    requests: ActivityRequests,
    /// Raised when a window asked or followed something, or when what the history reads moved: the
    /// desk draws a frame to take it.
    woken: Arc<AtomicBool>,
    /// The broker's end of the same activity, once this session serves the apps it runs.
    wire: RefCell<Option<Arc<ActivityWire>>>,
    /// The revision the model last served its reads at, as the apps following them were last told.
    served: Cell<Option<u64>>,
    /// What this device has already told the person about its lasting refusals: one notice per
    /// device and reason, never one per write or per pass.
    #[cfg(target_os = "linux")]
    told_refusals: RefCell<BTreeSet<ToldRefusal>>,
    /// What the desk's history says its changes were, as a model answered it.
    describer: RefCell<Describer>,
    /// Answers the model gave, waiting for the desk's thread to write them.
    described: Arc<Mutex<Vec<(Job, String)>>>,
    /// Whether a model is being asked right now: one request at a time keeps a long history from
    /// spending the person's provider on a burst.
    describing: Arc<AtomicBool>,
    /// The desk's changes the describer last read, so a pass with nothing new asks nothing.
    described_heads: RefCell<Vec<automerge::ChangeHash>>,
    /// When a model was last looked for: a provider the person configures while the session runs is
    /// taken up without a frame per lookup.
    looked_for_model: Cell<Option<Instant>>,
    /// What wakes the session's loop when a model answered off this thread.
    wake: Option<tilo_service::Wake>,
}

/// How often a session with nothing new on its desk looks again for a model the person may have
/// configured meanwhile.
const LOOK_FOR_MODEL: Duration = Duration::from_secs(30);

/// Something the history reads moved — a record an app wrote, an answer another of the person's
/// devices made: the desk draws a frame to read it, as it does for a window that asked, and the
/// session's loop wakes to serve it. Raised by a change alone, never by a pass or a clock.
fn ask_for_frame(woken: &AtomicBool, wake: Option<&tilo_service::Wake>) {
    woken.store(true, Ordering::Release);
    if let Some(wake) = wake {
        wake();
    }
}

impl Activity {
    /// This session's activity: the desk's document under `state` (kept in memory alone without
    /// one), the desk an older Tilo kept at `old_desk` brought over when this device has none, the
    /// records under `store` , and the person here.
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
        let maps = HistoryMaps::load(store.clone(), state.as_deref());
        Self(Rc::new(Inner {
            desk,
            #[cfg(target_os = "linux")]
            unlock,
            host: Rc::new(Host {
                person: person.clone(),
                maps,
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
                #[cfg(target_os = "linux")]
                woken: Arc::clone(&woken),
                #[cfg(target_os = "linux")]
                wake: wake.clone(),
            }),
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

    /// Show where a workspace's writes were refused from `engine` , the workspaces this device
    /// holds: the history rows read the verdicts this device recorded, and the one place the person
    /// hears of a lasting one.
    #[cfg(target_os = "linux")]
    fn attach_engine(&self, engine: Arc<crate::engine::Engine>) {
        *self.0.host.engine.borrow_mut() = Some(engine);
    }

    /// Tell the person about the refusals of this device's writes that waiting cannot change, once
    /// each: a device that will not take the write says why, because the change is not arriving
    /// there. A verdict that is gone is forgotten, so a device that refuses again later is heard
    /// again.
    #[cfg(target_os = "linux")]
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

    /// Open this device's workspaces and show them in the activity: what refused one of the
    /// history's writes, and what the devices call themselves.
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

    /// The desk this device left, as its document holds it: what the session starts from, and what
    /// became of the desk on disk when this build did not open it.
    pub fn opened_desk(&self) -> Kept {
        self.0.desk.opened()
    }

    /// What the windows ask of the activity and follow.
    pub fn requests(&self) -> ActivityRequests {
        self.0.requests.clone()
    }

    /// Serve the apps this session runs: `wire` reads what the model served,
    /// and is told when it served something new.
    #[cfg(target_os = "linux")]
    pub(crate) fn serve_apps(&self, wire: Arc<ActivityWire>) {
        *self.0.wire.borrow_mut() = Some(wire);
    }

    /// Serve the apps this session runs through `door` : what undoes an agent's run in the records
    /// it changed, as the person at the screen.
    #[cfg(target_os = "linux")]
    pub(crate) fn attach_door(&self, door: Arc<dyn crate::task_undo::UndoDoor>) {
        *self.0.host.door.borrow_mut() = Some(door);
    }

    /// Read the output of the command runs History shows from `content` , the records' content
    /// this device keeps.
    #[cfg(target_os = "linux")]
    pub(crate) fn attach_content(&self, content: Arc<crate::content::Plane>) {
        *self.0.host.content.borrow_mut() = Some(content);
    }

    /// Take part in `link` 's fabric: what the person's devices answer each other about unlocking a
    /// disk is kept in their own record here.
    #[cfg(target_os = "linux")]
    pub(crate) fn watch_fabric(&self, link: SharedLink) {
        self.0.unlock.watch(link);
    }

    /// Write what the person's fabric holds now into their own record,
    /// whether or not a window shows the history: an answer stands for
    /// minutes, so it is written while it is there to be read. An answer the
    /// record did not hold yet is a change of the history, and asks the desk
    /// for the frame that lists it.
    #[cfg(target_os = "linux")]
    pub(crate) fn keep_unlock(&self) {
        if self.0.unlock.absorb() {
            ask_for_frame(&self.0.woken, self.0.wake.as_ref());
        }
    }

    /// Serve what the activity holds on this pass: tell the apps following it that what they read
    /// may have moved — once the model served its reads at a revision they have not been told of —
    /// and tell the person about the refusals of this device's writes that will not change on their
    /// own.
    #[cfg(target_os = "linux")]
    pub(crate) fn serve(&self, notifications: &crate::notifications::SessionNotifications) {
        self.serve_following();
        self.tell_refusals(notifications);
    }

    /// Tell the apps following the activity that what they read may have moved.
    pub fn serve_following(&self) {
        let served = self.0.requests.served_revision();
        if served != self.0.served.get() {
            self.0.served.set(served);
            if let Some(wire) = self.0.wire.borrow().as_ref() {
                wire.changed();
            }
        }
    }

    /// Record the desk as `tree` holds it, at `time` (seconds since the epoch), by the person here:
    /// one change of the desk's document, and the document kept. Whether the desk changed at all.
    pub fn record_desk(
        &self,
        tree: &DesktopTree,
        time: i64,
        shows: &[(tilo_desktop_model::TabId, String)],
    ) -> bool {
        self.0.desk.record(tree, time, shows)
    }

    /// [`Self::record_desk`], and write even how the desk looks now: what ends the session, which
    /// the next one starts from.
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

    /// The resources the person opened from the history since this was last taken, each with the
    /// workspace it was opened in.
    pub fn take_openings(&self) -> Vec<(WorkspaceId, ui::HistoryResourceTarget)> {
        std::mem::take(&mut *self.0.openings.borrow_mut())
    }

    /// Whether a window asked the model something, or what the history reads moved, since this was
    /// last taken: the desk draws a frame to take it.
    pub fn take_woken(&self) -> bool {
        self.0.woken.swap(false, Ordering::AcqRel)
    }

    /// When this session next looks for a model, so a provider the person configures while it runs
    /// is taken up with no pass in between.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.0
            .looked_for_model
            .get()
            .and_then(|at| at.checked_add(LOOK_FOR_MODEL))
    }

    /// Say what the desk's changes were, and name the runs of them, when the desk moved since they
    /// were last read: one request at a time, off the desk's thread, written into the desk's
    /// document when the answer comes so every member reads the same words. Whether the document
    /// changed and the desk has something new to show.
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
        describer.set_enabled(self.0.host.models.is_some());
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
        let models = self.0.host.models.clone();
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

    /// The records the workspace `workspace_id` holds, as the store last read them: made when the
    /// history is first read for the workspace, and following them from then on.
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
                &self.0.host.writers.noted,
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
                let noted = self.0.host.writers.noted.clone();
                move |actor| noted(actor)
            });
            let read = move || read_workspace_records(reading.as_ref(), &by_workspace, &noted);
            let Ok(watch) = Watched::start("tilo-activity-records", directories, read) else {
                return read_workspace_records(store.as_ref(), &key, &self.0.host.writers.noted);
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
        Rc::clone(&self.0.host) as Rc<dyn ActivityHost>
    }

    fn register_authors(&self, authors: &mut ui::AuthorDirectory) {
        // Who this host's own mounts write as: a record's changes by the person who made them.
        (self.0.host.writers.register)(authors);
        // The names the desk's changes are known by: the person here, and whoever the desk came
        // from when it was read into its document.
        let known = self.0.desk.authors();
        let people = &*self.0.host.people;
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
        // What the fabric holds now is written before the record is read: a pass that hears an
        // answer is the pass that lists it. The desk is reading the history as it stands, so this
        // write needs no frame.
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
        self.0.host.pane.pane_code()
    }

    /// The person opened a resource from the history: its app shows it. No window here is bound to
    /// a record, so the session opens the app on the target on its next pass, after the frame that
    /// asked.
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

/// The directories that change when a record a workspace holds does: the workspace's index, where
/// its app names the records it holds, and each record's, where the app writes the document. A
/// record an app starts holding after this is read is picked up by its workspace's index; what the
/// app writes into it after that is read when a session reads the history again.
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

/// Every record a workspace holds, with the document its app last wrote. A record of an app this
/// host does not have is not one a window here shows, so it is not read.
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
                noted(&document.get_actor());
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

/// The records `package` 's index for `workspace` names, as the store holds it: `None` where the
/// workspace holds no index for the package, so nothing of its records is read here.
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

/// The document `package` keeps for record `record` in `workspace` , as its app last committed it:
/// `None` while the app holds changes it has not committed, so the history shows none of them.
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

/// What the model may ask of this session: who is looking, what they have seen, and the names of
/// the apps that hold the records.
struct Host {
    person: String,
    maps: HistoryMaps,
    apps: Option<Arc<dyn Apps>>,
    /// Who a principal is, as presence shows them.
    people: People,
    /// The writers of the records this host's mounts write.
    writers: Writers,
    /// Where the activity model's code is.
    pane: Rc<dyn Pane>,
    models: Option<Arc<dyn crate::platform::Models>>,
    /// The workspaces this device holds, once the session opened them: what refused one of the
    /// history's writes, and what the devices call themselves.
    #[cfg(target_os = "linux")]
    engine: RefCell<Option<Arc<crate::engine::Engine>>>,
    /// The session's action door, once its apps run here: the records an agent's run is undone in.
    #[cfg(target_os = "linux")]
    door: RefCell<Option<Arc<dyn crate::task_undo::UndoDoor>>>,
    /// The records' content this device keeps: what a command an agent ran printed.
    #[cfg(target_os = "linux")]
    content: RefCell<Option<Arc<crate::content::Plane>>>,
    #[cfg(target_os = "linux")]
    undos: crate::task_undo::Undos,
    /// What asks the desk for the frame that shows how an undo ended.
    #[cfg(target_os = "linux")]
    woken: Arc<AtomicBool>,
    #[cfg(target_os = "linux")]
    wake: Option<tilo_service::Wake>,
}

impl ActivityHost for Host {
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

    fn load_departures(&self) -> HashMap<u64, WorkspaceDeparture> {
        self.maps.map("departures")
    }

    fn save_departures(&self, departures: &HashMap<u64, WorkspaceDeparture>) {
        self.maps.save("departures", departures);
    }

    fn mutations_available(&self) -> bool {
        true
    }

    /// The desk's own layout is this session's to write, so undoing or restoring a change of it
    /// reaches the desk. A record's history is its app's to write, and this session holds no writer
    /// for one.
    fn writable(&self, origin: &HistoryOrigin) -> bool {
        matches!(origin, HistoryOrigin::DesktopTree)
    }

    #[cfg(target_os = "linux")]
    fn record_content(&self, workspace: &str, app: &str, hash: &str) -> Option<Vec<u8>> {
        self.content.borrow().as_ref()?.read(workspace, app, hash)
    }

    fn app_display_name(&self, app: &str) -> Option<String> {
        let name = self.apps.as_ref()?.name(app);
        (!name.is_empty()).then_some(name)
    }

    #[cfg(target_os = "linux")]
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

    fn undo_task_run(&self, undo: TaskRunUndo) -> Result<(), String> {
        // Undone by each changed document's own history, as the person at the screen.
        #[cfg(target_os = "linux")]
        {
            let Some(door) = self.door.borrow().as_ref().map(Arc::clone) else {
                return Err(tilo_desktop_surface::i18n::t(
                    "activity-undo-task-unavailable",
                ));
            };
            let woken = Arc::clone(&self.woken);
            let wake = self.wake.clone();
            return self.undos.start(
                door,
                self.person.clone(),
                undo.workspace,
                undo.invocation,
                undo.task_id,
                undo.steps,
                move || ask_for_frame(&woken, wake.as_ref()),
            );
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
            match self.undos.poll(invocation)? {
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

/// This device's private view of the workspaces' activity: the changes it has seen, when it left a
/// workspace, and the morning sources it chose. Kept in one record of this device's store, and
/// written as it changes; the file this device kept it in before is read once, into the record.
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

    /// Keep `values` as `key` 's map, for this session at once and for the next one once the store
    /// has written it.
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

    /// A desk of one workspace with Notes open, its activity served from the desk's own log the way
    /// this session serves it.
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

        /// One pass of the desk: what the windows asked is answered. The desk as it stood is
        /// recorded first, as the pass before this one recorded it, so what this pass draws is what
        /// the desk is.
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

    /// The activity opens from its command as a window of the desk, and it lists the workspace's
    /// real changes: a rename the person made here.
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

    /// Nothing the history reads moved, so no pass asks the desk for a frame: the model is read
    /// again when a change says so, never on a clock and never because a frame was drawn.
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

    /// An arrangement action is one step of the desk's own history: the person takes exactly that
    /// step back, and the desk it takes them back to is staged for the pass to apply.
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

    /// A version restored from the history reaches the desk: the session takes up what the model
    /// wrote and the desk is as it was, and the restore is itself a change of it.
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
