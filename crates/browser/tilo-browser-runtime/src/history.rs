//! The page desktop's workspace history, for the surface's Activity pane.
//!
//! The desktop's own history is a desktop log
//! (`tilo_desktop_surface::history_log`): in a certified room the room's
//! replicated desktop document, every peer's changes in it; otherwise the
//! log this profile keeps beside its stored desktop. The records the
//! windows show carry their own histories, read from detached copies of
//! the open documents.
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash as _, Hasher as _};
use std::rc::Rc;

use durable_store::{DurableStore, StorageKey, WriteOp};
use tilo_desktop_model::{DesktopTree, TabId, TileContent, WorkspaceId};
use tilo_desktop_surface::activity::{
    ActivityHost, ActivitySnapshot, DetachedRecordHistory, WorkspaceDeparture,
    WorkspaceSeenFrontier,
};
use tilo_desktop_surface::history_log;
use ui::automerge::{AutoCommit, transaction::Transactable as _};
use ui::record::RecordCheckout;

/// Seconds since the epoch, as history stamps changes.
pub(crate) fn now_seconds() -> i64 {
    ui::time::SystemTime::now()
        .duration_since(ui::time::SystemTime::UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
        .unwrap_or_default()
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
        crate::desktop::application(app).map(|definition| definition.title.into())
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

    /// `workspace`'s windows and the records they show.
    fn bound(&self, tree: &DesktopTree, workspace: WorkspaceId) -> Vec<(String, RecordCheckout)> {
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
        let mut bound: Vec<(String, RecordCheckout)> = self
            .shared
            .bound_records()
            .into_iter()
            .filter_map(|(tab, checkout)| Some((apps.get(&tab)?.clone(), checkout)))
            .filter(|(app, checkout)| seen.insert((app.clone(), checkout.id.clone())))
            .collect();
        bound.sort_by(|a, b| (&a.0, a.1.id.as_str()).cmp(&(&b.0, b.1.id.as_str())));
        bound
    }

    fn read_record<R>(
        &self,
        app: &str,
        checkout: &RecordCheckout,
        read: impl FnOnce(&AutoCommit) -> R,
    ) -> Option<R> {
        let stores = self.shared.stores.borrow();
        let store = stores
            .iter()
            .find(|store| store.package_and_home().0 == app)?;
        store.read(&checkout.id, read)
    }
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
