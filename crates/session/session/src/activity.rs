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
//! slow to read must not hold up a frame. What this device has seen of a
//! workspace is its own, kept in one file ([`HistoryMaps`]): only the person
//! here marks it, and it never reaches a shared record.
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
use tilo_desktop_model::{DesktopTree, RoomId, Workspace, WorkspaceId};
use tilo_desktop_surface::HistorySource;
use tilo_desktop_surface::activity::describer::{Describer, Job};
use tilo_desktop_surface::activity::{
    ActivityHost, ActivitySnapshot, DetachedRecordHistory, HistoryOrigin, WorkspaceDeparture,
    WorkspaceSeenFrontier, morning,
};
use tilo_desktop_surface::history_log;
use tilo_desktop_surface::session::{ActivityRequests, activity_requests};
use tilo_files::native::Watched;
use tilo_lock::MutexRecover as _;
use ui::record::RecordCheckout;

use crate::activity_provider::ActivityWire;
use crate::desk_doc::{DeskDoc, Kept};
use crate::lists::Lists;

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

/// A workspace's activity, as this session serves it: to the desk's own
/// model, and to the apps that ask through the broker.
#[derive(Clone)]
pub(crate) struct Activity(Rc<Inner>);

struct Inner {
    /// The desk's document, whose changes are its history, and where it is
    /// kept.
    desk: DeskDoc,
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
    openings: RefCell<Vec<(RoomId, ui::HistoryResourceTarget)>>,
    /// The windows' end of what they ask of the activity, and what they
    /// follow.
    requests: ActivityRequests,
    /// Raised when a window asked or followed something: the desk draws a
    /// frame to take it.
    woken: Arc<AtomicBool>,
    /// The broker's end of the same activity, once this session serves the
    /// apps it runs.
    wire: RefCell<Option<Arc<ActivityWire>>>,
    /// The revision the model last served its reads at, as the apps
    /// following them were last told.
    served: Cell<Option<u64>>,
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
}

/// How often a session with nothing new on its desk looks again for a model
/// the person may have configured meanwhile.
const LOOK_FOR_MODEL: Duration = Duration::from_secs(30);

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
    ) -> Self {
        let woken = Arc::new(AtomicBool::new(false));
        let requests = activity_requests(Arc::clone(&woken));
        Self(Rc::new(Inner {
            desk: DeskDoc::load(state, old_desk, &person),
            host: Rc::new(NativeActivityHost {
                person: person.clone(),
                maps: HistoryMaps::load(state),
                lists,
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
            describer: RefCell::default(),
            described: Arc::default(),
            describing: Arc::default(),
            described_heads: RefCell::default(),
            looked_for_model: Cell::default(),
        }))
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

    /// Tell the apps following the activity that what they read may have
    /// moved: once the model served its reads at a revision they have not
    /// been told of.
    pub(crate) fn tell_followers(&self) {
        let served = self.0.requests.served_revision();
        if served == self.0.served.get() {
            return;
        }
        self.0.served.set(served);
        if let Some(wire) = self.0.wire.borrow().as_ref() {
            wire.changed();
        }
    }

    /// Record the desk as `tree` holds it, at `time` (seconds since the
    /// epoch), by the person here: one change of the desk's document, and the
    /// document kept. Whether the desk changed at all.
    pub(crate) fn record_desk(&self, tree: &DesktopTree, time: i64) -> bool {
        self.0.desk.record(tree, time)
    }

    /// [`Self::record_desk`], and write even how the desk looks now: what ends
    /// the session, which the next one starts from.
    pub(crate) fn record_desk_now(&self, tree: &DesktopTree, time: i64) -> bool {
        self.0.desk.record_now(tree, time)
    }

    /// The workspace a change the model wrote back leaves the desk at, if
    /// one is waiting.
    pub(crate) fn take_restored(&self) -> Option<DesktopTree> {
        self.0.restored.borrow_mut().take()
    }

    /// The resources the person opened from the history since this was last
    /// taken, each with the workspace it was opened in.
    pub(crate) fn take_openings(&self) -> Vec<(RoomId, ui::HistoryResourceTarget)> {
        std::mem::take(&mut *self.0.openings.borrow_mut())
    }

    /// Whether a window asked the model something since this was last
    /// taken: the desk draws a frame to answer it.
    pub(crate) fn take_woken(&self) -> bool {
        self.0.woken.swap(false, Ordering::AcqRel)
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
        std::thread::spawn(move || {
            let answer = crate::activity_models::ask(&job).unwrap_or_default();
            answers.lock_recover().push((job, answer));
            describing.store(false, Ordering::Release);
            // The desk draws a frame to write the answer where it belongs.
            woken.store(true, Ordering::Release);
        });
        wrote
    }

    /// The `RoomId` of `workspace`, as `tree` holds it.
    fn room_of(tree: &DesktopTree, workspace: WorkspaceId) -> Option<RoomId> {
        tree.workspaces
            .iter()
            .find(|held| held.id == workspace)
            .map(Workspace::room)
    }

    /// The records the workspace `workspace_id` holds, as the worker last
    /// read them: made when the history is first read for the workspace, and
    /// following them from then on.
    fn records_of(&self, workspace_id: RoomId) -> Option<Rc<Watched<Vec<DetachedRecordHistory>>>> {
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
        watch.on_change(move || {
            moved.fetch_add(1, Ordering::AcqRel);
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
        workspace: WorkspaceId,
    ) -> Option<ActivitySnapshot> {
        Self::room_of(tree, workspace)?;
        // The desk as it stands is the newest change of it: a workspace made
        // this pass is in the document the pane reads, not only in the tree.
        self.record_desk(tree, now_seconds());
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
        workspace: WorkspaceId,
    ) -> Vec<DetachedRecordHistory> {
        let Some(workspace_id) = Self::room_of(tree, workspace) else {
            return Vec::new();
        };
        let Some(watch) = self.records_of(workspace_id) else {
            return Vec::new();
        };
        let held = tree.workspace_records_held.get(&workspace_id);
        let records = watch.get();
        records
            .iter()
            .filter(|record| held.is_none_or(|held| held.contains_key(record.checkout.id.as_str())))
            .cloned()
            .collect()
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
    }

    fn revision(&self) -> u64 {
        use std::hash::{Hash as _, Hasher as _};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.0.desk.heads().hash(&mut hasher);
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
        let workspace_id = tree.active_workspace().map(Workspace::room)?;
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
            tracing::debug!(%error, %package, %record, "a record is not read");
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
            tracing::debug!(%error, %package, %record, "a record does not load");
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

    fn load_departures(&self) -> HashMap<u64, WorkspaceDeparture> {
        self.maps.map("departures")
    }

    fn save_departures(&self, departures: &HashMap<u64, WorkspaceDeparture>) {
        self.maps.save("departures", departures);
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
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
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
        workspace: WorkspaceId,
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
            };
            let tree = seed_tree(&mut minter, "Studio", vec![notes]);
            let workspace = tree.active;
            let (mut surface, handle) = DesktopSurface::new(tree, 7, Box::new(Tiles));
            let activity = Activity::new(None, None, "ada".to_string(), None, Lists::default());
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
            self.activity.record_desk(&tree, now_seconds());
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
