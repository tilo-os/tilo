//! The page desktop's workspace history, for the surface's Activity pane.
//!
//! The desktop's own history is a desktop log
//! (`tilo_desktop_surface::history_log`): in a certified room the room's
//! replicated desktop document, every peer's changes in it; otherwise the
//! log this profile keeps beside its stored desktop. The records the
//! windows show carry their own histories, read from detached copies of
//! the open documents.
//!
//! The pane's own code is linked in, unless the `deferred-activity` feature
//! leaves it to the bundling crate: the browser desktop hands over a loader
//! ([`defer_activity_pane`]) that fetches it as its own module the first
//! time someone opens Activity, so the start module does not carry it.
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::hash::{Hash as _, Hasher as _};
use std::pin::Pin;
use std::rc::Rc;

use durable_store::{DurableStore, StorageKey, WriteOp};
use tilo_desktop_model::{DesktopTree, TabId, TileContent, WorkspaceId};
use tilo_desktop_surface::activity::{
    ActivityHost, ActivitySnapshot, DetachedRecordHistory, WorkspaceDeparture,
    WorkspaceSeenFrontier,
};
use tilo_desktop_surface::history_log;
use tilo_desktop_surface::{ActivityPaneCode, OpenActivity};
use ui::automerge::{AutoCommit, transaction::Transactable as _};
use ui::record::RecordCheckout;

/// Fetches the Activity pane's code; `None` when it could not.
pub type LoadActivity = fn() -> Pin<Box<dyn Future<Output = Option<OpenActivity>>>>;

/// Where the page has the Activity pane's code.
#[derive(Clone, Copy)]
enum PaneCode {
    Ready(OpenActivity),
    /// Fetched when the pane first opens.
    Deferred(LoadActivity),
    Loading,
    /// The last fetch failed: that request is dismissed, the next one
    /// fetches again.
    Failed(LoadActivity),
}

thread_local! {
    static PANE_CODE: RefCell<PaneCode> = RefCell::new(linked_pane());
    /// The desktops that asked for the pane while its code loads.
    static WAITING: RefCell<Vec<crate::desktop::Shared>> = const { RefCell::new(Vec::new()) };
}

#[cfg(not(feature = "deferred-activity"))]
fn linked_pane() -> PaneCode {
    PaneCode::Ready(tilo_desktop_surface::open_activity)
}

/// No loader yet: the bundling crate supplies one before it mounts a
/// desktop, and Activity stays unavailable without it.
#[cfg(feature = "deferred-activity")]
fn linked_pane() -> PaneCode {
    fn missing() -> Pin<Box<dyn Future<Output = Option<OpenActivity>>>> {
        Box::pin(std::future::ready(None))
    }
    PaneCode::Deferred(missing)
}

/// Fetch the Activity pane's code with `load` the first time it opens,
/// instead of linking it in (the `deferred-activity` feature).
pub fn defer_activity_pane(load: LoadActivity) {
    PANE_CODE.with_borrow_mut(|code| *code = PaneCode::Deferred(load));
}

fn wait_for_pane(desktop: &crate::desktop::Shared) {
    WAITING.with_borrow_mut(|waiting| {
        if !waiting
            .iter()
            .any(|other| std::sync::Arc::ptr_eq(&other.woken, &desktop.woken))
        {
            waiting.push(desktop.clone());
        }
    });
}

/// The pane's code for `desktop`, which is woken for its update pass once
/// code still loading has arrived (or failed to).
fn activity_pane(desktop: &crate::desktop::Shared) -> ActivityPaneCode {
    let code = PANE_CODE.with_borrow(|code| *code);
    match code {
        PaneCode::Ready(open) => ActivityPaneCode::Ready(open),
        PaneCode::Loading => {
            wait_for_pane(desktop);
            ActivityPaneCode::Loading
        }
        PaneCode::Deferred(load) => {
            PANE_CODE.with_borrow_mut(|code| *code = PaneCode::Loading);
            wait_for_pane(desktop);
            wasm_bindgen_futures::spawn_local(async move {
                let loaded = load().await;
                PANE_CODE.with_borrow_mut(|code| {
                    *code = loaded.map_or(PaneCode::Failed(load), PaneCode::Ready);
                });
                for desktop in WAITING.take() {
                    desktop.wake();
                }
                crate::browser::wake_desktops();
            });
            ActivityPaneCode::Loading
        }
        PaneCode::Failed(load) => {
            PANE_CODE.with_borrow_mut(|code| *code = PaneCode::Deferred(load));
            ActivityPaneCode::Unavailable
        }
    }
}

/// What the pane may ask of the page: who is looking, what they have seen,
/// application names.
struct PageActivityHost {
    author: String,
    store: Rc<dyn DurableStore>,
    room: String,
}

impl PageActivityHost {
    fn key(&self, what: &str) -> StorageKey {
        StorageKey::record(crate::desktop::DESKTOP_PACKAGE, self.room.clone(), what)
    }

    fn load<T: serde::de::DeserializeOwned + Default>(&self, what: &str) -> T {
        self.store
            .read(&self.key(what))
            .ok()
            .flatten()
            .and_then(|stored| serde_json::from_slice(&stored.bytes).ok())
            .unwrap_or_default()
    }

    fn save<T: serde::Serialize>(&self, what: &str, value: &T) {
        if let Ok(bytes) = serde_json::to_vec(value) {
            let _ = self
                .store
                .write(&self.key(what), WriteOp::Replace(bytes), None);
        }
    }
}

impl ActivityHost for PageActivityHost {
    fn local_user(&self) -> String {
        self.author.clone()
    }
    fn load_frontiers(&self) -> HashMap<u64, WorkspaceSeenFrontier> {
        self.load("activity-seen")
    }
    fn save_frontiers(&self, frontiers: &HashMap<u64, WorkspaceSeenFrontier>) {
        self.save("activity-seen", frontiers);
    }
    fn load_departures(&self) -> HashMap<u64, WorkspaceDeparture> {
        self.load("activity-departures")
    }
    fn save_departures(&self, departures: &HashMap<u64, WorkspaceDeparture>) {
        self.save("activity-departures", departures);
    }
    fn app_display_name(&self, app: &str) -> Option<String> {
        crate::desktop::known_application(app).map(Into::into)
    }
}

/// The page desktop's [`tilo_desktop_surface::HistorySource`].
pub(crate) struct PageHistory {
    peer: Option<u32>,
    author: String,
    shared: crate::desktop::Shared,
    /// The profile's own log, kept by `desktop::Persistence`.
    log: Rc<RefCell<AutoCommit>>,
    host: Rc<PageActivityHost>,
    /// Who each desktop change's actor is, from the last read.
    authors: BTreeMap<String, String>,
    partial: bool,
}

impl PageHistory {
    pub fn new(
        room: &str,
        peer: Option<u32>,
        author: String,
        store: Rc<dyn DurableStore>,
        shared: crate::desktop::Shared,
        log: Rc<RefCell<AutoCommit>>,
    ) -> Self {
        Self {
            peer,
            author: author.clone(),
            shared,
            log,
            host: Rc::new(PageActivityHost {
                author,
                store,
                room: room.to_owned(),
            }),
            authors: BTreeMap::new(),
            partial: false,
        }
    }

    /// The room's replicated desktop when the page has one, else the
    /// profile's log.
    fn desktop_log(&self) -> AutoCommit {
        self.peer
            .and_then(crate::replication::desktop_history)
            .unwrap_or_else(|| self.log.borrow().clone())
    }

    fn bound(&self, tree: &DesktopTree, workspace: WorkspaceId) -> Vec<(String, RecordCheckout)> {
        bound(&self.shared, tree, workspace)
            .into_iter()
            .map(|(_, app, checkout)| (app, checkout))
            .collect()
    }

    fn read_record<R>(
        &self,
        app: &str,
        checkout: &RecordCheckout,
        read: impl FnOnce(&AutoCommit) -> R,
    ) -> Option<R> {
        read_record(&self.shared, app, checkout, read)
    }
}

/// `workspace`'s windows and the records they show: the first window of
/// each record, its app and the record.
fn bound(
    shared: &crate::desktop::Shared,
    tree: &DesktopTree,
    workspace: WorkspaceId,
) -> Vec<(TabId, String, RecordCheckout)> {
    let apps: HashMap<TabId, String> = tree
        .workspaces
        .iter()
        .filter(|w| w.id == workspace)
        .flat_map(|w| &w.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .filter_map(|tab| match &tab.tile {
            TileContent::LocalApp { app_id, .. } => Some((tab.id, app_id.clone())),
            _ => None,
        })
        .collect();
    let mut seen = HashSet::new();
    let mut bound: Vec<(TabId, String, RecordCheckout)> = shared
        .bound_records()
        .into_iter()
        .filter_map(|(tab, checkout)| Some((tab, apps.get(&tab)?.clone(), checkout)))
        .filter(|(_, app, checkout)| seen.insert((app.clone(), checkout.id.clone())))
        .collect();
    bound.sort_by(|a, b| (&a.1, a.2.id.as_str()).cmp(&(&b.1, b.2.id.as_str())));
    bound
}

fn read_record<R>(
    shared: &crate::desktop::Shared,
    app: &str,
    checkout: &RecordCheckout,
    read: impl FnOnce(&AutoCommit) -> R,
) -> Option<R> {
    let stores = shared.stores.borrow();
    let store = stores
        .iter()
        .find(|store| store.package_and_home().0 == app)?;
    store.read(&checkout.id, read)
}

/// The most of its own steps a page reports as what it is doing.
const MAX_OWN_ACTIONS: usize = 16;
/// The bound, in characters, of each title a page reports.
const MAX_OWN_TITLE_CHARS: usize = 96;

/// One step this page's person took, as "who is here" reports it: the
/// titles of what they did, the record and the app, never the content.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct OwnAction {
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

fn bounded_title(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(MAX_OWN_TITLE_CHARS)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// What `author` did in the records the active workspace's windows show
/// since `since_ms` (Unix milliseconds), oldest first and at most
/// [`MAX_OWN_ACTIONS`]. A step names its action without the item it
/// touched, so a row's or a paragraph's words never leave the page.
pub(crate) fn own_recent_actions(
    shared: &crate::desktop::Shared,
    tree: &DesktopTree,
    author: &str,
    since_ms: u64,
) -> Vec<OwnAction> {
    let titles: HashMap<TabId, &str> = tree
        .workspaces
        .iter()
        .flat_map(|w| &w.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .map(|tab| (tab.id, tab.title.as_str()))
        .collect();
    let mut authors = ui::AuthorDirectory::new();
    crate::page_authors::register(&mut authors, &[]);
    authors.set_reader(author);
    let mut actions = Vec::new();
    for (tab, app, checkout) in bound(shared, tree, tree.active) {
        let Some(mut doc) = read_record(shared, &app, &checkout, |doc| {
            (doc.pending_ops() == 0).then(|| doc.clone())
        })
        .flatten() else {
            continue;
        };
        let entries = ui::HistorySource::new()
            .without_field_paths()
            .without_record_genesis()
            .with_authors(authors.clone())
            .with_max_entries(MAX_OWN_ACTIONS)
            .entries(&mut doc);
        let app_name = crate::desktop::known_application(&app).unwrap_or_default();
        for entry in entries {
            let mine = entry.author.relation == ui::widget::AuthorRelation::Reader
                || entry.author.name == author;
            let at_ms = entry
                .timestamp
                .duration_since(ui::time::UNIX_EPOCH)
                .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
            if !mine || at_ms < since_ms {
                continue;
            }
            let (action, resource) = match &entry.context {
                Some(context) => {
                    let mut step = context.clone();
                    step.item = None;
                    (
                        ui::history_source::sentence::step_summary(&step),
                        context.resource.as_ref().map(|r| r.label.clone()),
                    )
                }
                None => (None, None),
            };
            let action = bounded_title(&action.unwrap_or_else(|| "Edited".to_owned()));
            if action.is_empty() {
                continue;
            }
            let resource = resource
                .filter(|label| !label.trim().is_empty())
                .or_else(|| titles.get(&tab).map(|title| (*title).to_owned()))
                .unwrap_or_default();
            actions.push(OwnAction {
                id: hex::encode(entry.id.0),
                action,
                resource: bounded_title(&resource),
                app: bounded_title(app_name),
                at_ms,
            });
        }
    }
    actions.sort_by(|a, b| a.at_ms.cmp(&b.at_ms).then_with(|| a.id.cmp(&b.id)));
    actions.dedup_by(|a, b| a.id == b.id);
    // Typing commits a step every second or so: a run of the same action on
    // the same resource is one thing the person did, named by its newest step.
    actions.reverse();
    actions.dedup_by(|older, newer| {
        older.action == newer.action && older.resource == newer.resource && older.app == newer.app
    });
    actions.reverse();
    if actions.len() > MAX_OWN_ACTIONS {
        actions.drain(..actions.len().saturating_sub(MAX_OWN_ACTIONS));
    }
    actions
}

impl tilo_desktop_surface::HistorySource for PageHistory {
    fn workspace_history(
        &mut self,
        tree: &DesktopTree,
        workspace: WorkspaceId,
    ) -> Option<ActivitySnapshot> {
        let room = tree.workspaces.iter().find(|w| w.id == workspace)?.room;
        let history = history_log::room_history(self.desktop_log(), room);
        self.authors = history.authors;
        self.partial = history.truncated;
        let mut desktop = tree.clone();
        desktop.active = workspace;
        ActivitySnapshot::new(desktop, history.doc).ok()
    }

    fn record_histories(
        &mut self,
        tree: &DesktopTree,
        workspace: WorkspaceId,
    ) -> Vec<DetachedRecordHistory> {
        self.bound(tree, workspace)
            .into_iter()
            .map(|(app, checkout)| {
                let document = self
                    .read_record(&app, &checkout, |doc| {
                        crate::page_authors::note_mount(Some(doc.get_actor()), &self.author, None);
                        (doc.pending_ops() == 0).then(|| doc.clone())
                    })
                    .flatten();
                DetachedRecordHistory {
                    app,
                    checkout,
                    document,
                }
            })
            .collect()
    }

    fn history_is_partial(&self) -> bool {
        self.partial
    }

    fn activity_pane(&self) -> ActivityPaneCode {
        activity_pane(&self.shared)
    }

    fn host(&self) -> Rc<dyn ActivityHost> {
        Rc::clone(&self.host) as Rc<dyn ActivityHost>
    }

    fn register_authors(&self, authors: &mut ui::AuthorDirectory) {
        crate::page_authors::register(authors, &[]);
        history_log::register_authors(authors, &self.authors);
    }

    fn resource_tab(
        &self,
        tree: &DesktopTree,
        target: &ui::HistoryResourceTarget,
    ) -> Option<TabId> {
        let active = tree.active;
        self.shared
            .bound_records()
            .into_iter()
            .filter(|(_, checkout)| checkout.id.as_str() == target.id)
            .map(|(tab, _)| tab)
            .find(|tab| {
                tree.workspaces
                    .iter()
                    .filter(|w| w.id == active)
                    .flat_map(|w| &w.strips)
                    .flat_map(|strip| &strip.window_groups)
                    .any(|group| group.tabs.iter().any(|t| t.id == *tab))
            })
    }

    /// The heads of every record the active workspace's windows show.
    fn revision(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.peer
            .and_then(crate::replication::desktop_history_heads)
            .unwrap_or_else(|| self.log.borrow().base_heads())
            .hash(&mut hasher);
        let mut tree_records = self.shared.bound_records();
        tree_records.sort_by_key(|(tab, _)| *tab);
        for (_, checkout) in tree_records {
            checkout.id.as_str().hash(&mut hasher);
            let stores = self.shared.stores.borrow();
            for store in stores.iter() {
                if let Some(heads) = store.read(
                    &checkout.id,
                    ui::automerge::transaction::Transactable::base_heads,
                ) {
                    heads.hash(&mut hasher);
                }
            }
        }
        hasher.finish()
    }
}
