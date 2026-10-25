//! The session desktop in the page: `tilo-desktop-surface` over a
//! `tilo-desktop-model` tree, with every tab an in-process application
//! component registered through its `ApplicationDefinition`.
//!
//! The tree and the record each tab shows are kept per room in the profile's
//! durable store ([`Persistence`]) and committed to `IndexedDB` with the
//! records themselves, so a reload restores the same windows on the same
//! documents. A profile with no stored desktop is seeded with a window per
//! requested application.
pub(crate) type PersonIdentity = (String, (u8, u8, u8));

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tilo_sort::CompactSort as _;

use durable_store::{DurableStore, StorageKey, WriteOp};
use tilo_desktop_model::{DesktopTree, IdMinter, Tab, TabId, TileContent, Workspace};
use tilo_desktop_surface::navigation::Application;
use tilo_desktop_surface::{DesktopHandle, DesktopSurface, TileSource};
use tilo_lock::MutexRecover as _;
use ui::application::{
    AppComponent, ApplicationDefinition, HostContext, HostContextInputs, LaunchTarget, RestoreInput,
};
use ui::application_view::InstanceViewStore;
use ui::automerge::AutoCommit;
use ui::permissions::{PermissionContext, UserId};
use ui::record::{RecordCheckout, RecordId, RecordOwners, RecordStore};
use ui::widget::TileHost;

/// Applications the page can open, in launcher order: every application
/// the bundle registered from its manifests (`ui::application::registry`),
/// else the built-in Scratch, so a runtime without apps still opens one. A
/// bundle whose apps are all still deferred has apps, just none loaded yet.
pub(crate) fn applications() -> Vec<&'static ApplicationDefinition> {
    let mut applications = if ui::application::registry::catalogue().is_empty() {
        vec![crate::scratch::definition()]
    } else {
        ui::application::registry::applications()
    };
    // The composer is an application of the bundle when it registered it
    // (fetched on first open); otherwise it is linked in here.
    if !applications
        .iter()
        .any(|app| app.app_id == tilo_desktop::feedback::PACKAGE)
        && let Some(code) = crate::feedback_composer::code()
    {
        applications.push((code.definition)());
    }
    applications
}

/// An application the page can open by id whose code is loaded, Scratch
/// included: windows a desktop saved before keep their application.
pub(crate) fn application(app: &str) -> Option<&'static ApplicationDefinition> {
    applications()
        .into_iter()
        .chain(std::iter::once(crate::scratch::definition()))
        .find(|definition| definition.app_id == app)
}

/// Every application the page can open, as `(id, title)` in launcher
/// order: those loaded ([`applications`]) and those whose code is fetched
/// when they first open (`ui::application::registry::defer`).
pub(crate) fn known_applications() -> Vec<(&'static str, &'static str)> {
    let mut known = ui::application::registry::catalogue();
    for definition in applications() {
        if !known.iter().any(|(id, _)| *id == definition.app_id) {
            known.push((definition.app_id, definition.title));
        }
    }
    known
}

/// The title of an application the page can open by id, loaded or not.
/// Whether `permissions` let this person open `app` here, as the workspace
/// broker decides it on a native desktop: an installed app, and leave to
/// change the app's part of the workspace.
///
/// # Errors
/// An app this desktop doesn't have, or one this person may not open.
pub(crate) fn may_open(
    permissions: &ui::permissions::PermissionContext,
    app: &str,
) -> Result<(), tilo_actions::remote::ActionHostError> {
    known_application(app).ok_or(tilo_actions::remote::ActionHostError::UnknownAction)?;
    if !permissions.check(&ui::permissions::PermissionScope::app(
        app,
        ui::permissions::ActionKind::Undoable,
    )) {
        return Err(tilo_actions::remote::ActionHostError::NotPermitted);
    }
    Ok(())
}

pub(crate) fn known_application(app: &str) -> Option<&'static str> {
    let scratch = crate::scratch::definition();
    known_applications()
        .into_iter()
        .chain(std::iter::once((scratch.app_id, scratch.title)))
        .find_map(|(id, title)| (id == app).then_some(title))
}

pub(crate) fn catalogue() -> Vec<Application> {
    known_applications()
        .into_iter()
        .map(|(id, title)| Application {
            id: id.into(),
            label: title.into(),
        })
        .collect()
}

/// Everything a tab's component is prepared with.
pub(crate) struct Inputs {
    pub feedback: Option<tilo_desktop::feedback::transport::Startup>,
    pub owners: RecordOwners,
    pub views: Rc<InstanceViewStore>,
    pub device: String,
    /// Seeds this desktop's share of the id space: the certified device
    /// key when the mount has a peer (every browser's profile namespace may
    /// be the same string), else the device namespace.
    pub salt: String,
    pub room: String,
    pub author: String,
    pub permissions: Rc<RefCell<PermissionContext>>,
    pub models: ui::models::Models,
    pub provider_admin: ui::models::ProviderAdministration,
    /// The peer whose room this desktop shows, when it has one.
    pub peer: Option<u32>,
}

/// The person who made each strip or opened each window, by its id. Kept
/// with the desktop: who may close, move or rename each one
/// ([`tilo_desktop_model::ownership`]).
pub(crate) type Authors = Rc<RefCell<BTreeMap<u64, String>>>;

/// The roles a room answers a page's person holds, in the order
/// [`Standing`] counts them.
const STANDINGS: [tilo_action_policy::Role; 5] = [
    tilo_action_policy::Role::Viewer,
    tilo_action_policy::Role::Commenter,
    tilo_action_policy::Role::Editor,
    tilo_action_policy::Role::Moderator,
    tilo_action_policy::Role::Owner,
];

/// The role this desktop's person holds where it is: the owner of a desktop
/// of their own, and in a room what the room last answered (not known
/// until it has). Shared with the room's replication, which asks.
#[derive(Clone, Debug)]
pub(crate) struct Standing(Arc<std::sync::atomic::AtomicU8>);

impl Default for Standing {
    /// A desktop of the person's own: theirs to arrange.
    fn default() -> Self {
        let standing = Self::unknown();
        standing.set(Some(tilo_action_policy::Role::Owner));
        standing
    }
}

impl Standing {
    /// A role not known yet.
    pub fn unknown() -> Self {
        Self(Arc::default())
    }

    /// The role, as [`STANDINGS`] position plus one; 0 while not known.
    pub fn code(&self) -> u8 {
        self.0.load(Ordering::Relaxed)
    }

    pub fn role(&self) -> Option<tilo_action_policy::Role> {
        self.code()
            .checked_sub(1)
            .and_then(|at| STANDINGS.get(usize::from(at)).copied())
    }

    /// Hold `role` (`None`: not known); whether that changed it.
    pub fn set(&self, role: Option<tilo_action_policy::Role>) -> bool {
        let code = role
            .and_then(|role| STANDINGS.iter().position(|held| *held == role))
            .and_then(|at| u8::try_from(at).ok())
            .map_or(0, |at| at.saturating_add(1));
        self.0.swap(code, Ordering::Relaxed) != code
    }

    /// The same standing as `other` from now on: one answer reaches both.
    pub fn share(&mut self, other: &Self) {
        self.0 = Arc::clone(&other.0);
    }

    /// `person` as the desktop's policy asks about them.
    pub fn of<'a>(&self, person: &'a str) -> tilo_desktop_model::commands::Standing<'a> {
        tilo_desktop_model::commands::Standing {
            person,
            role: self.role(),
            layout_shared: true,
            members: None,
        }
    }
}

/// What a desktop installs a feedback startup with.
pub(crate) struct LateFeedback {
    author: String,
    store: Rc<dyn DurableStore>,
    models: ui::models::Models,
}

impl LateFeedback {
    fn install(
        &self,
        shared: &Shared,
        startup: tilo_desktop::feedback::transport::Startup,
    ) -> Result<(), String> {
        if startup.reporter().principal()
            != &tilo_actions::remote::CallerPrincipal::Person(self.author.clone())
        {
            return Err("feedback reporter does not own this profile".into());
        }
        startup
            .install(Rc::clone(&self.store), &mut shared.feedback.borrow_mut())
            .map_err(|error| error.to_string())?;
        // The model this page may use also looks over reports before they
        // are sent.
        if let Some(feedback) = shared.feedback.borrow().as_ref() {
            feedback.services().set_models(self.models.clone());
        }
        Ok(())
    }
}

impl Shared {
    /// Install the feedback startup the page registered after the desktop
    /// mounted; whether it did just now.
    pub(crate) fn adopt_feedback(&self) -> bool {
        let Some(peer) = self.late_feedback.borrow().as_ref().map(|(peer, _)| *peer) else {
            return false;
        };
        let Some(startup) = crate::feedback_delivery::take_startup(peer) else {
            return false;
        };
        let Some((_, late)) = self.late_feedback.borrow_mut().take() else {
            return false;
        };
        match late.install(self, startup) {
            Ok(()) => true,
            Err(error) => {
                web_sys::console::warn_1(&format!("feedback: {error}").into());
                false
            }
        }
    }
}

/// State the mount and the tile source share.
#[derive(Clone, Default)]
pub(crate) struct Shared {
    /// Trusted host custody, never configured from a launch URL or draft.
    pub feedback: Rc<RefCell<Option<tilo_desktop::feedback::PrivateFeedback>>>,
    /// What installs feedback once the page holds a destination, when the
    /// desktop mounted before it did ([`Shared::adopt_feedback`]).
    late_feedback: Rc<RefCell<Option<(u32, LateFeedback)>>>,
    /// Every package's record owner opened so far, flushed with the mount.
    pub stores: Rc<RefCell<Vec<RecordStore>>>,
    pub images: Rc<RefCell<Vec<ui::image_store::HostImageLoader>>>,
    /// The window each image loader draws for, by its place in `images`
    /// ([`crate::page_content::ImageOwners`]).
    pub image_owners: crate::page_content::ImageOwners,
    /// Set by any tab component's wake; the host relayouts before drawing.
    pub woken: Arc<AtomicBool>,
    pub owner_wakes: ui::owner_wake::OwnerWakeQueue,
    /// The applications whose components woke the mount since it last
    /// looked, for the frame metrics.
    wakers: Arc<std::sync::Mutex<Vec<&'static str>>>,
    /// The record each tab shows, once its component created or opened it.
    records: Rc<RefCell<HashMap<TabId, RecordCheckout>>>,
    /// A tab bound a record since the desktop was last saved.
    records_changed: Rc<Cell<bool>>,
    /// Tabs that could not open yet: a peer's desktop named a record
    /// before the record itself arrived, or the tab's application is still
    /// loading.
    waiting: Rc<RefCell<std::collections::HashSet<TabId>>>,
    /// Record ids of tabs whose application is not loaded yet: the record's
    /// kind comes with the application, so they are bound once it is.
    unbound: Rc<RefCell<HashMap<TabId, String>>>,
    /// Who made each strip and opened each window, by strip or tab id.
    pub authors: Authors,
    /// The workspace renamed since the mount last looked, for a notice when
    /// someone else did it.
    pub renames: Rc<RefCell<Vec<tilo_desktop_surface::history_log::WorkspaceRename>>>,
    /// Whether the room lets this desktop's person close and move what
    /// others made, as it last answered ([`crate::replication`] asks).
    pub moderates: Arc<AtomicBool>,
    /// The role the room last answered this desktop's person holds, which
    /// the desktop's policy decides what to offer by
    /// ([`tilo_desktop_model::commands::policy`]).
    pub standing: Standing,
    /// The room keeps no member's layout (a public room refused this
    /// desktop's): its windows and desks are the person's own, sent to
    /// nobody, and the room's later layout no longer replaces them.
    pub own_layout: Rc<Cell<bool>>,
    /// The page only watches the room, as its spectator: nobody there sees
    /// this desktop's person, so it offers nothing about how they are seen.
    pub watching: Rc<Cell<bool>>,
    /// The room's members as its governance lists them, once heard: what
    /// someone no longer among them made is released, anyone's here to
    /// close ([`tilo_desktop_model::ownership::released`]).
    pub members: Rc<RefCell<Option<std::collections::BTreeSet<String>>>>,
    /// The person's accounts with the services this room is connected to,
    /// read from the connectors' room records by the room's host
    /// ([`PageRoomActions`]).
    pub linked: tilo_app::linked_accounts::LinkedAccounts,
    /// The room actions this desktop asked the room's host to run.
    pub room_actions: PageRoomActions,
    /// Applications whose code is being fetched.
    loading: Rc<RefCell<crate::app_load::Loads>>,
    /// Applications whose code failed to load since their tabs last asked.
    failed: Rc<RefCell<std::collections::HashSet<String>>>,
    /// An application finished loading (or failed to) since the mount last
    /// looked: its waiting tabs are built again.
    arrived: Rc<Cell<bool>>,
    /// The windows showing another member's legacy windows, by tab.
    peer_windows: Rc<RefCell<HashMap<TabId, PeerWindow>>>,
    /// Legacy windows asked for and not opened yet, by key.
    pending_peer_windows: Rc<RefCell<HashMap<String, PeerWindow>>>,
    /// Legacy windows the person closed while they still streamed: not
    /// opened again, and reported once so the page closes them at their
    /// source.
    dismissed_peer_windows: Rc<RefCell<std::collections::HashSet<String>>>,
    /// Of those, the ones not reported yet.
    newly_dismissed: Rc<RefCell<Vec<String>>>,
    /// The applications the node beside the host runs that this desktop
    /// does not (legacy ones, Tilo apps that need a host), which the person
    /// can open there and see here as legacy windows.
    node_apps: Rc<RefCell<Vec<NodeApp>>>,
    /// Node applications the person asked to open, by id, for the page to
    /// ask the node.
    node_app_requests: Rc<RefCell<Vec<String>>>,
    /// Pages a host streams as display lists (`tilo-paint`), by window key:
    /// drawn here by the toolkit instead of decoded as video.
    paint_pages: Rc<RefCell<HashMap<String, Arc<tilo_browser_peer::remote_page::RemotePage>>>>,
}

/// Launch name of an application the node runs.
pub(crate) const NODE_APP_LAUNCH: &str = "tilo.node-app:";

/// What search says of an application the node runs.
const NODE_APP_DESCRIPTION: &str = "Runs on the node";

/// An application on the node: its launch id there, its name, and for a
/// Tilo application its package id.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub(crate) struct NodeApp {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub app: Option<String>,
}

/// Launch name of the window showing the legacy window `key`.
pub(crate) const PEER_WINDOW_LAUNCH: &str = "tilo.peer-window:";

/// Another member's legacy window, as the page streams it: its key (the
/// page's name for the stream), app id, title and codec.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub(crate) struct PeerWindow {
    pub key: String,
    pub app: String,
    pub title: String,
    #[serde(default)]
    pub codec: String,
}

impl PeerWindow {
    /// A page this desktop draws itself rather than a video it is shown.
    fn is_paint(&self) -> bool {
        self.codec == tilo_relay_protocol::legacy_window::PAINT
    }
}

impl Shared {
    /// `person` as the desktop's policy asks about them here, among the
    /// room's `members` (borrowed from [`Self::members`]).
    pub fn standing_of<'a>(
        &self,
        person: &'a str,
        members: Option<&'a std::collections::BTreeSet<String>>,
    ) -> tilo_desktop_model::commands::Standing<'a> {
        tilo_desktop_model::commands::Standing {
            layout_shared: !self.own_layout.get(),
            members,
            ..self.standing.of(person)
        }
    }

    /// Ask the host to run the surface's update pass before the next frame
    /// (queued navigation, a replaced tree).
    pub fn wake(&self) {
        self.woken.store(true, Ordering::Release);
    }

    /// A record arrived: build the tabs still waiting for theirs again.
    pub fn retry_waiting(&self, handle: &DesktopHandle) {
        let waiting = std::mem::take(&mut *self.waiting.borrow_mut());
        if waiting.is_empty() {
            return;
        }
        for tab in waiting {
            handle.rebuild(tab);
        }
        self.wake();
    }

    /// Build `tab` again on the next frame, as the tabs of an application
    /// that just arrived are: a window whose app crashed, reopened.
    pub fn reopen(&self, tab: TabId) {
        self.waiting.borrow_mut().insert(tab);
        self.arrived.set(true);
        self.wake();
    }

    /// Whether an application finished loading since the last call.
    pub fn take_arrived(&self) -> bool {
        self.arrived.replace(false)
    }

    /// Fetch a deferred application's code; its tabs are built again once
    /// it is in, or show that it failed.
    fn load(&self, app_id: &str, notify: &Arc<dyn Fn() + Send + Sync>) {
        let Some((id, _)) = ui::application::registry::catalogue()
            .into_iter()
            .find(|(id, _)| *id == app_id)
        else {
            return;
        };
        let Some(attempt) = self.loading.borrow_mut().start(id) else {
            return;
        };
        let shared = self.clone();
        let fetched = Arc::clone(notify);
        wasm_bindgen_futures::spawn_local(async move {
            let loaded = ui::application::registry::load(id).await.is_some();
            // Code that arrives after the bound is registered all the same,
            // so opening the window again shows the app at once.
            if shared.loading.borrow_mut().settle(id, attempt) {
                shared.settle_load(id, loaded, &fetched);
            }
        });
        // A fetch that never ends must not leave its windows holding their
        // place for ever: past the bound they say it could not be loaded.
        let shared = self.clone();
        let bounded = Arc::clone(notify);
        wasm_bindgen_futures::spawn_local(async move {
            crate::browser::sleep_ms(crate::app_load::LOAD_BOUND_MS).await;
            if shared.loading.borrow_mut().settle(id, attempt) {
                web_sys::console::warn_1(&wasm_bindgen::JsValue::from_str(&format!(
                    "{id}: its code did not arrive in time"
                )));
                shared.settle_load(id, false, &bounded);
            }
        });
    }

    /// The fetch of `id`'s code settled: its waiting windows are built again
    /// and show the app, or that it could not be loaded.
    fn settle_load(&self, id: &str, loaded: bool, notify: &Arc<dyn Fn() + Send + Sync>) {
        if !loaded {
            self.failed.borrow_mut().insert(id.to_owned());
        }
        self.arrived.set(true);
        self.wake();
        notify();
    }

    /// The room's store of `definition`'s records: the one a window of it
    /// opened, or opened now and flushed and replicated with the mount's.
    fn room_store(
        &self,
        owners: &RecordOwners,
        room: &str,
        definition: &'static ApplicationDefinition,
    ) -> Result<RecordStore, String> {
        let home = (definition.app_id.to_owned(), room.to_owned());
        if let Some(store) = self
            .stores
            .borrow()
            .iter()
            .find(|store| store.package_and_home() == home)
        {
            return Ok(store.clone());
        }
        // Clones share the owner registry: the same writer either way.
        let store = owners
            .clone()
            .open(definition.app_id, room)
            .map_err(|error| error.to_string())?;
        self.stores.borrow_mut().push(store.clone());
        Ok(store)
    }

    pub fn take_wake(&self) -> bool {
        self.woken.swap(false, Ordering::AcqRel)
    }

    /// The applications whose components woke the mount since the last call.
    pub fn take_wakers(&self) -> Vec<&'static str> {
        std::mem::take(&mut *self.wakers.lock_recover())
    }

    /// The page behind a painted window, created on first use.
    pub fn paint_page(&self, key: &str) -> Arc<tilo_browser_peer::remote_page::RemotePage> {
        Arc::clone(
            self.paint_pages
                .borrow_mut()
                .entry(key.to_owned())
                .or_insert_with(tilo_browser_peer::remote_page::RemotePage::new),
        )
    }

    /// Input on painted windows since the last call, by window key.
    pub fn take_paint_input(
        &self,
    ) -> Vec<(String, Vec<tilo_relay_protocol::legacy_window::InputEvent>)> {
        self.paint_pages
            .borrow()
            .iter()
            .map(|(key, page)| (key.clone(), page.take_input()))
            .filter(|(_, events)| !events.is_empty())
            .collect()
    }

    /// The legacy window `tab` shows, if it shows one.
    pub fn peer_window_of(&self, tab: TabId) -> Option<String> {
        self.peer_windows
            .borrow()
            .get(&tab)
            .map(|window| window.key.clone())
    }

    /// The tab showing the legacy window `key`, if one does.
    pub fn peer_window_tab(&self, key: &str) -> Option<TabId> {
        self.peer_windows
            .borrow()
            .iter()
            .find_map(|(tab, window)| (window.key == key).then_some(*tab))
    }

    /// Reconcile the windows showing legacy windows with `wanted`: the
    /// launch names of the ones to open, and the tabs to close (no longer
    /// wanted, or restored from a saved desktop that streamed them before).
    pub fn reconcile_peer_windows(
        &self,
        tree: &tilo_desktop_model::DesktopTree,
        wanted: Vec<PeerWindow>,
    ) -> (Vec<String>, Vec<TabId>) {
        let live: std::collections::HashSet<_> = tree
            .workspaces
            .iter()
            .flat_map(|w| &w.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .map(|tab| tab.id)
            .collect();
        let mut open = self.peer_windows.borrow_mut();
        self.note_dismissed(&mut open, &live);
        let mut dismissed = self.dismissed_peer_windows.borrow_mut();
        dismissed.retain(|key| wanted.iter().any(|window| window.key == *key));
        let wanted: Vec<PeerWindow> = wanted
            .into_iter()
            .filter(|window| !dismissed.contains(&window.key))
            .collect();
        drop(dismissed);
        // A page no longer shared stops being kept.
        self.paint_pages.borrow_mut().retain(|key, _| {
            wanted
                .iter()
                .any(|window| window.key == *key && window.is_paint())
        });
        let mut close = Vec::new();
        for tab in tree
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .filter(|tab| matches!(tab.tile, TileContent::PeerWindow { .. }))
        {
            if open
                .get(&tab.id)
                .is_none_or(|window| !wanted.iter().any(|wanted| wanted.key == window.key))
            {
                close.push(tab.id);
            }
        }
        let mut pending = self.pending_peer_windows.borrow_mut();
        pending.clear();
        let mut launch = Vec::new();
        for window in wanted {
            if open.values().any(|open| open.key == window.key) {
                continue;
            }
            launch.push(format!("{PEER_WINDOW_LAUNCH}{}", window.key));
            pending.insert(window.key.clone(), window);
        }
        (launch, close)
    }

    /// The applications the node runs, as the page listed them, but for
    /// the Tilo applications this desktop opens itself.
    pub fn set_node_apps(&self, mut apps: Vec<NodeApp>) {
        apps.retain(|node| {
            node.app
                .as_deref()
                .is_none_or(|package| known_application(package).is_none())
        });
        *self.node_apps.borrow_mut() = apps;
    }

    /// The search entries of the applications the node runs. An app neither
    /// this page nor the node can run is not offered at all.
    fn node_search_entries(&self) -> Vec<tilo_desktop_search::providers::AppEntry> {
        self.node_apps
            .borrow()
            .iter()
            .map(|node| tilo_desktop_search::providers::AppEntry {
                id: format!("{NODE_APP_LAUNCH}{}", node.id),
                title: node.title.clone(),
                category: node
                    .app
                    .as_deref()
                    .and_then(tilo_desktop_search::availability::category_for)
                    .map(str::to_owned),
                description: Some(NODE_APP_DESCRIPTION.into()),
            })
            .collect()
    }

    /// The node applications the person asked to open since the last call.
    pub fn take_node_app_requests(&self) -> Vec<String> {
        std::mem::take(&mut *self.node_app_requests.borrow_mut())
    }

    /// The applications this desktop opens: the bundle's, then the node's.
    pub fn catalogue(&self) -> Vec<Application> {
        catalogue()
            .into_iter()
            .chain(self.node_apps.borrow().iter().map(|app| Application {
                id: format!("{NODE_APP_LAUNCH}{}", app.id),
                label: app.title.clone(),
            }))
            .collect()
    }

    /// Forget the legacy window `tab` showed.
    /// Who holds each window of `tree` this desktop's person `me` may not
    /// close or move, named as `person` (by user root) names people here:
    /// the windows the desktop's policy keeps for whoever opened them
    /// ([`tilo_desktop_model::commands::policy::may_rearrange_window`]),
    /// everyone's but their own unless they moderate the room. Another
    /// member's legacy windows, which only this page shows, are its own to
    /// close. A window nobody opened is the room's, named `room`.
    pub fn holders(
        &self,
        tree: &DesktopTree,
        me: &str,
        room: &str,
        person: &mut dyn FnMut(&str) -> PersonIdentity,
    ) -> HashMap<TabId, ui::widget::TabHolder> {
        if self.moderates.load(Ordering::Relaxed) {
            return HashMap::new();
        }
        let members = self.members.borrow();
        let standing = self.standing_of(me, members.as_ref());
        let authors = self.authors.borrow();
        let local = self.peer_windows.borrow();
        tree.workspaces
            .iter()
            .flat_map(|w| &w.strips)
            .flat_map(|s| &s.window_groups)
            .flat_map(|g| &g.tabs)
            .filter(|tab| !local.contains_key(&tab.id))
            .filter(|tab| {
                tilo_desktop_model::commands::policy::may_rearrange_window(
                    &standing, &authors, tab.id,
                )
                .is_err()
            })
            .map(|tab| {
                let (name, rgb) = match authors.get(&tab.id) {
                    Some(author) => person(author),
                    None => (room.to_owned(), ui::presence::user_color(room)),
                };
                (tab.id, ui::widget::TabHolder { name, rgb })
            })
            .collect()
    }

    pub fn closed_peer_window(&self, tab: TabId) {
        self.peer_windows.borrow_mut().remove(&tab);
    }

    /// Move the windows whose tabs are gone from `live` (the person closed
    /// them) to the dismissed ones.
    fn note_dismissed(
        &self,
        open: &mut HashMap<TabId, PeerWindow>,
        live: &std::collections::HashSet<TabId>,
    ) {
        let mut dismissed = self.dismissed_peer_windows.borrow_mut();
        let mut newly = self.newly_dismissed.borrow_mut();
        open.retain(|tab, window| {
            if live.contains(tab) {
                return true;
            }
            if dismissed.insert(window.key.clone()) {
                newly.push(window.key.clone());
            }
            false
        });
    }

    /// The legacy windows the person closed since the last call, by key:
    /// the page asks their sources to close them.
    pub fn take_dismissed_peer_windows(
        &self,
        tree: &tilo_desktop_model::DesktopTree,
    ) -> Vec<String> {
        let live: std::collections::HashSet<_> = tree
            .workspaces
            .iter()
            .flat_map(|w| &w.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .map(|tab| tab.id)
            .collect();
        self.note_dismissed(&mut self.peer_windows.borrow_mut(), &live);
        std::mem::take(&mut *self.newly_dismissed.borrow_mut())
    }

    pub fn bound_records(&self) -> Vec<(TabId, RecordCheckout)> {
        self.records
            .borrow()
            .iter()
            .map(|(tab, checkout)| (*tab, checkout.clone()))
            .collect()
    }

    /// The record `tab` shows as the room's live layer names it: its
    /// application and the record id, empty for the package's primary
    /// record.
    pub fn live_record_of(&self, tree: &DesktopTree, tab: TabId) -> Option<(String, String)> {
        let app = tree
            .workspaces
            .iter()
            .flat_map(|w| &w.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .find(|candidate| candidate.id == tab)
            .and_then(|tab| match &tab.tile {
                TileContent::LocalApp { app_id, .. } => Some(app_id.clone()),
                _ => None,
            })?;
        let records = self.records.borrow();
        let id = &records.get(&tab)?.id;
        let record = if id.is_package_primary() {
            String::new()
        } else {
            id.as_str().to_owned()
        };
        Some((app, record))
    }
}

/// The Settings app, which the page lends the person's linked accounts.
const SETTINGS: &str = "so.tilo.settings";

/// An action of a room record the desktop asked the room's host to run, as
/// the page reads it from the mount's status (`room-action`) and answers it
/// ([`crate::browser::desktop_exports::desktop_room_action_answer`]).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct RoomActionRequest {
    pub id: u32,
    pub app: String,
    pub action: String,
    pub arguments: serde_json::Map<String, serde_json::Value>,
}

/// The room actions the desktop asked for and not answered yet.
#[derive(Default)]
struct RoomActionQueue {
    next: u32,
    outbox: Vec<RoomActionRequest>,
    waiting: HashMap<u32, Box<dyn FnOnce(tilo_app::linked_accounts::ActionAnswer)>>,
}

/// The desktop's door to the room's records, run by the room's host: the
/// page asks the host (`peer.actions`) to run each action as the person
/// and hands the answer back. No app code runs in the page for it, so a
/// connector the page never loads (Discord) answers too.
#[derive(Clone, Default)]
pub(crate) struct PageRoomActions {
    queue: Rc<RefCell<RoomActionQueue>>,
    /// The peer whose room this desktop shows, for the records it holds.
    peer: Option<u32>,
}

impl PageRoomActions {
    /// The requests the page has to send, once.
    pub fn take_requests(&self) -> Vec<RoomActionRequest> {
        std::mem::take(&mut self.queue.borrow_mut().outbox)
    }

    pub fn answer(&self, id: u32, answer: tilo_app::linked_accounts::ActionAnswer) {
        let done = self.queue.borrow_mut().waiting.remove(&id);
        if let Some(done) = done {
            done(answer);
        }
    }
}

/// The person's linked accounts in this desktop's room, asked of the
/// room's host through `shared`'s room actions; an answer wakes the mount.
/// `shared` is the mount's before it holds them, so they hold no cycle.
fn linked_accounts(
    shared: &Shared,
    notify: &Arc<dyn Fn() + Send + Sync>,
) -> tilo_app::linked_accounts::LinkedAccounts {
    let (woken, notify) = (shared.clone(), Arc::clone(notify));
    tilo_app::linked_accounts::LinkedAccounts::new(Rc::new(
        tilo_app::linked_accounts::RoomAccounts::new(
            Rc::new(shared.room_actions.clone()),
            Rc::new(move || {
                woken.wake();
                notify();
            }),
        ),
    ))
}

impl tilo_app::linked_accounts::RoomRecordActions for PageRoomActions {
    fn apps(&self) -> Vec<String> {
        let room_apps = self
            .peer
            .map(crate::replication::records)
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, record)| record.is_empty())
            .map(|(app, _)| app)
            .filter(|app| app != crate::replication::DESKTOP_APP);
        // An app this page can open runs here, not on the room's host: no
        // connector, so its record is not asked for a reading.
        let here = known_applications();
        tilo_app::linked_accounts::connector_apps(room_apps, |app| {
            here.iter().any(|(id, _)| *id == app)
        })
    }

    fn invoke(
        &self,
        app: &str,
        action: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
        done: Box<dyn FnOnce(tilo_app::linked_accounts::ActionAnswer)>,
    ) {
        let mut queue = self.queue.borrow_mut();
        queue.next = queue.next.wrapping_add(1);
        let id = queue.next;
        queue.waiting.insert(id, done);
        queue.outbox.push(RoomActionRequest {
            id,
            app: app.to_owned(),
            action: action.to_owned(),
            arguments,
        });
    }
}

struct Tiles {
    inputs: Inputs,
    shared: Shared,
    stores: HashMap<&'static str, RecordStore>,
    notify: Arc<dyn Fn() + Send + Sync>,
}

impl Tiles {
    fn store(&mut self, definition: &'static ApplicationDefinition) -> Result<RecordStore, String> {
        if let Some(store) = self.stores.get(definition.app_id) {
            return Ok(store.clone());
        }
        let store = self
            .shared
            .room_store(&self.inputs.owners, &self.inputs.room, definition)?;
        self.stores.insert(definition.app_id, store.clone());
        Ok(store)
    }

    /// The app `tab` shows once it can be built here, or the line the
    /// window shows instead.
    fn ready_app<'t>(&self, tab: &'t Tab) -> Result<&'t str, Unready> {
        if let TileContent::PeerWindow { label, .. } = &tab.tile {
            // The page draws the stream over this window's content.
            return Err(Unready::Notice(
                if self.shared.peer_windows.borrow().contains_key(&tab.id) {
                    format!("Connecting to {label}…")
                } else {
                    format!("{label} is no longer shared")
                },
            ));
        }
        let TileContent::LocalApp { app_id, .. } = &tab.tile else {
            return Err(Unready::Notice(
                "This window has no application in the browser".into(),
            ));
        };
        if self.shared.failed.borrow_mut().remove(app_id.as_str()) {
            return Err(Unready::Notice(format!(
                "{} could not be loaded. Check the connection, then open it again.",
                tab.title
            )));
        }
        if ui::application::registry::is_deferred(app_id) {
            self.load(app_id);
            self.shared.waiting.borrow_mut().insert(tab.id);
            return Err(Unready::Arriving);
        }
        Ok(app_id)
    }

    fn component(&mut self, tab: &Tab) -> Result<AppComponent, Unready> {
        let app_id = self.ready_app(tab)?;
        let definition = application(app_id)
            .ok_or_else(|| format!("{app_id} is not available in the browser"))?;
        let unbound = self.shared.unbound.borrow_mut().remove(&tab.id);
        if let Some(id) = unbound.as_deref().and_then(RecordId::parse) {
            let checkout = RecordCheckout::new(id, definition.record_kind.clone());
            self.shared.records.borrow_mut().insert(tab.id, checkout);
        }
        if definition.app_id == tilo_desktop::feedback::PACKAGE {
            return Ok(self.private_feedback(tab.id)?);
        }
        let store = self.store(definition)?;
        let (target, named_new) = self.launch_target(definition, &store, tab.id)?;
        if named_new.is_some() || matches!(target, LaunchTarget::PrimaryNew) {
            self.may_start_room_record(tab)?;
        }
        let restore = self
            .inputs
            .views
            .load(tab.id)
            .map_err(|error| error.to_string())?
            .map_or(RestoreInput::None, RestoreInput::Local);
        let inputs = HostContextInputs {
            records: store,
            views: Rc::clone(&self.inputs.views),
            device_id: self.inputs.device.clone(),
            instance_id: tab.id,
            permissions: Rc::clone(&self.inputs.permissions),
            author: UserId(self.inputs.author.clone()),
            action_context: tilo_actions::ActionContext::default(),
        };
        let mut context = match named_new {
            Some(id) => HostContext::prepare_new_record(definition, inputs, id),
            None => HostContext::prepare(definition, inputs, &target),
        }
        .map_err(|error| error.to_string())?;
        context.provide_models(self.inputs.models.clone());
        context.provide_content(crate::page_content());
        // Only registered control panels receive the administration capability.
        if crate::host_services::provider_admin_app(definition.app_id) {
            crate::host_services::provide_provider_admin(
                &mut context,
                self.inputs.provider_admin.clone(),
            )?;
        }
        // Settings shows the person's linked accounts beside their others.
        if definition.app_id == SETTINGS {
            context.lend(self.shared.linked.clone());
        }
        let destination = Rc::clone(&self.shared.images);
        let first_loader = self.shared.images.borrow().len();
        let mut component = ui::ImageStore::with_host_factory_with_max_dimension(
            Rc::new(move |max_dimension| {
                let (image, loader) = ui::ImageStore::host_driven_with_max_dimension(max_dimension);
                destination.borrow_mut().push(loader);
                image
            }),
            || definition.instantiate(context, target, restore),
        )
        .map_err(|error| error.to_string())?;
        crate::page_content::own_loaders(
            &self.shared.image_owners,
            first_loader..self.shared.images.borrow().len(),
            definition.app_id,
            &component.record().id,
        );
        let woken = Arc::clone(&self.shared.woken);
        let wakers = Arc::clone(&self.shared.wakers);
        let notify = Arc::clone(&self.notify);
        let app_id = definition.app_id;
        component
            .mount_with_owner_queue(&self.shared.owner_wakes, move |_| {
                woken.store(true, Ordering::Release);
                let mut wakers = wakers.lock_recover();
                if !wakers.contains(&app_id) {
                    wakers.push(app_id);
                }
                drop(wakers);
                notify();
            })
            .map_err(|error| error.to_string())?;
        let record = component.record().clone();
        let previous = self
            .shared
            .records
            .borrow_mut()
            .insert(tab.id, record.clone());
        if previous.is_none_or(|previous| previous.id != record.id) {
            self.shared.records_changed.set(true);
        }
        Ok(component)
    }
}

/// Why a window cannot show its app yet.
enum Unready {
    /// The app's code is still being fetched.
    Arriving,
    /// What the window says instead.
    Notice(String),
}

impl From<String> for Unready {
    fn from(detail: String) -> Self {
        Self::Notice(detail)
    }
}

impl Tiles {
    /// Fetch a deferred application's code; its tabs are built again once
    /// it is in, or show that it failed.
    fn load(&self, app_id: &str) {
        self.shared.load(app_id, &self.notify);
    }

    fn private_feedback(&self, instance: TabId) -> Result<AppComponent, String> {
        // Only reached once the composer's code is in: a deferred composer's
        // window holds its place with the opening card until then (`ready_app`).
        let code = crate::feedback_composer::code()
            .ok_or_else(|| "Feedback could not be loaded".to_owned())?;
        let services = self
            .shared
            .feedback
            .borrow()
            .as_ref()
            .map(tilo_desktop::feedback::PrivateFeedback::services);
        let mut component =
            (code.prepare)(services, &self.inputs.device, &self.inputs.author, instance)
                .map_err(|error| error.to_string())?;
        let woken = Arc::clone(&self.shared.woken);
        let notify = Arc::clone(&self.notify);
        component
            .mount_with_owner_queue(&self.shared.owner_wakes, move |_| {
                woken.store(true, Ordering::Release);
                notify();
            })
            .map_err(|error| error.to_string())?;
        // Its ephemeral draft owner never enters Shared.stores/records:
        // neither replication nor desktop restore can publish raw text.
        Ok(component)
    }

    /// Whether this page may start the room's own record of `tab`'s app,
    /// which this page does not hold yet: in a room, only someone who
    /// writes there starts it. Anyone else's window waits for the room's
    /// record (it opens once it arrives, or once the room says they write)
    /// instead of showing a fresh one of its own as if it were the room's.
    fn may_start_room_record(&self, tab: &Tab) -> Result<(), Unready> {
        if self.shared.room_actions.peer.is_none() {
            return Ok(());
        }
        let notice = match self.shared.standing.role() {
            Some(role) if role.includes(tilo_action_policy::Role::Editor) => return Ok(()),
            None => format!("Opening {}…", tab.title),
            Some(_) => format!(
                "{} has nothing in this room yet. It shows here once someone who writes in the room starts it.",
                tab.title
            ),
        };
        self.shared.waiting.borrow_mut().insert(tab.id);
        Err(Unready::Notice(notice))
    }

    /// The record `tab` opens: its saved one, the room's own record, or a
    /// new one.
    fn launch_target(
        &self,
        definition: &'static ApplicationDefinition,
        store: &RecordStore,
        tab: TabId,
    ) -> Result<(LaunchTarget, Option<RecordId>), String> {
        let saved = self.shared.records.borrow().get(&tab).cloned();
        match saved {
            Some(checkout) => Ok((
                LaunchTarget::Record {
                    checkout,
                    reveal: None,
                },
                None,
            )),
            None if definition.keeps_room_record() => Ok((
                definition
                    .unsaved_target(store)
                    .map_err(|error| error.to_string())?,
                None,
            )),
            None => self.unbound_target(definition, store, tab),
        }
    }

    /// What a tab no record is bound to opens: the room's default record
    /// while no other window of the app shows it (the record a native
    /// launch into the room opens too), otherwise a new one. The second
    /// value names the default record when it has to be created.
    fn unbound_target(
        &self,
        definition: &'static ApplicationDefinition,
        store: &RecordStore,
        tab: TabId,
    ) -> Result<(LaunchTarget, Option<RecordId>), String> {
        let default = RecordCheckout::new(RecordId::room_default(), definition.record_kind.clone());
        let taken = self
            .shared
            .records
            .borrow()
            .iter()
            .any(|(other, checkout)| *other != tab && checkout.id == default.id);
        if taken {
            return Ok((LaunchTarget::New, None));
        }
        match store.try_open_record(&default) {
            Ok(_) => Ok((
                LaunchTarget::Record {
                    checkout: default,
                    reveal: None,
                },
                None,
            )),
            Err(ui::record::RecordOpenError::MissingRecord) => {
                Ok((LaunchTarget::New, Some(default.id)))
            }
            Err(error) => Err(error.to_string()),
        }
    }
}

impl TileSource for Tiles {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        let shared = self.shared.clone();
        let id = tab.id;
        if let Some(notice) =
            crate::crash::quarantined(tab.id, &tab.title, move || shared.reopen(id))
        {
            return TileHost::new(notice);
        }
        // A painted page is a window this desktop draws, like its own apps.
        let painted = self
            .shared
            .peer_windows
            .borrow()
            .get(&tab.id)
            .filter(|window| window.is_paint())
            .map(|window| window.key.clone());
        if let Some(key) = painted {
            let view = ui::widget::WebView::new(self.shared.paint_page(&key))
                .with_test_id("peer-page")
                .with_accessible_name(tab.title.clone());
            return TileHost::new(ui::widget::TileContent::App(Box::new(view)));
        }
        match self.component(tab) {
            Ok(component) => {
                self.shared.waiting.borrow_mut().remove(&tab.id);
                TileHost::from_component(component)
            }
            // The app's code is on its way: its window holds the place with
            // the opening card, and the app takes it over once built.
            Err(Unready::Arriving) => TileHost::new(ui::widget::TileContent::Arriving),
            Err(Unready::Notice(detail)) => {
                if self.shared.records.borrow().contains_key(&tab.id) {
                    self.shared.waiting.borrow_mut().insert(tab.id);
                }
                TileHost::new(ui::widget::TileContent::Notice {
                    title: tab.title.clone(),
                    detail,
                })
            }
        }
    }

    fn launch(&mut self, _: &Workspace, id: TabId, app: &str) -> Option<Tab> {
        if let Some(node_app) = app.strip_prefix(NODE_APP_LAUNCH) {
            // Opened on the node; its window arrives as a legacy window.
            if self
                .shared
                .node_apps
                .borrow()
                .iter()
                .any(|known| known.id == node_app)
            {
                self.shared
                    .node_app_requests
                    .borrow_mut()
                    .push(node_app.to_owned());
                self.shared.wake();
            }
            return None;
        }
        if let Some(key) = app.strip_prefix(PEER_WINDOW_LAUNCH) {
            let window = self.shared.pending_peer_windows.borrow_mut().remove(key)?;
            let tab = Tab {
                id,
                title: window.title.clone(),
                tile: TileContent::PeerWindow {
                    app_id: window.app.clone(),
                    label: window.title.clone(),
                    note: String::new(),
                    missing: None,
                },
                app_title: None,
            };
            self.shared.peer_windows.borrow_mut().insert(id, window);
            return Some(tab);
        }
        let title = known_application(app)?;
        self.shared
            .authors
            .borrow_mut()
            .insert(id, self.inputs.author.clone());
        Some(app_tab(id, app, title))
    }

    fn search_applications(&self) -> Vec<tilo_desktop_search::providers::AppEntry> {
        web_applications()
            .chain(self.shared.node_search_entries())
            .collect()
    }
}

/// The applications this page offers to open itself: those of the bundle
/// that run on the web.
fn web_applications() -> impl Iterator<Item = tilo_desktop_search::providers::AppEntry> {
    known_applications()
        .into_iter()
        .filter(|(id, _)| tilo_desktop_search::availability::runs_on(id, &["web"]))
        .map(|(id, title)| tilo_desktop_search::providers::AppEntry {
            id: id.into(),
            title: title.into(),
            category: None,
            description: None,
        })
}

fn app_tab(id: TabId, app: &str, title: &str) -> Tab {
    Tab {
        id,
        title: title.into(),
        tile: TileContent::LocalApp {
            app_id: app.into(),
            label: title.into(),
            note: String::new(),
        },
        app_title: None,
    }
}

/// This device's share of the desktop id space, so ids it mints never
/// collide with another device's once trees merge, and never fall in the
/// reserved namespace ([`tilo_desktop_model::ids::RESERVED_SALT`]) or in
/// salt `0`, which has none.
fn peer_salt(device: &str) -> u16 {
    // FNV-1a folded to 16 bits: stable across builds and platforms.
    let mut hash: u32 = 0x811c_9dc5;
    for byte in device.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    let folded = (hash >> 16) ^ (hash & 0xffff);
    tilo_desktop_model::ids::device_salt(u16::try_from(folded).unwrap_or(u16::MAX))
}

/// Where a room's desktop lives in the profile. Changing it orphans stored
/// desktops.
pub(crate) const DESKTOP_PACKAGE: &str = "so.tilo.browser-desktop";

/// The stored form of a room's desktop.
#[derive(serde::Serialize, serde::Deserialize)]
struct StoredDesktop {
    #[serde(deserialize_with = "DesktopTree::deserialize_via_json")]
    tree: DesktopTree,
    /// `(tab, record id)` for every tab whose component bound a record.
    records: Vec<(TabId, String)>,
    /// Who made each strip and opened each window still on the desktop.
    #[serde(default)]
    authors: BTreeMap<u64, String>,
}

/// Saves a mounted desktop to the profile whenever its tree or its tabs'
/// records change. Writes are accepted by the durable store and committed
/// to `IndexedDB` by the page's durable host.
pub(crate) struct Persistence {
    store: Rc<dyn DurableStore>,
    key: StorageKey,
    saved_revision: Cell<u64>,
    records_changed: Rc<Cell<bool>>,
    records: Rc<RefCell<HashMap<TabId, RecordCheckout>>>,
    unbound: Rc<RefCell<HashMap<TabId, String>>>,
    authors: Authors,
    /// Every change of this device's desktop, for Activity
    /// (`tilo_desktop_surface::history_log`).
    pub history: Rc<RefCell<AutoCommit>>,
    history_key: StorageKey,
    history_dirty: Cell<bool>,
    pub notifications: tilo_desktop_surface::DesktopNotifications,
    notifications_key: StorageKey,
    saved_notifications_revision: Cell<u64>,
    author: String,
}

impl Persistence {
    /// Write the desktop if it changed since the last save.
    ///
    /// # Errors
    /// The store refused the write; the next change retries.
    pub fn save(&self, handle: &DesktopHandle) -> Result<(), String> {
        let notifications_revision = self.notifications.revision();
        if notifications_revision != self.saved_notifications_revision.get() {
            // Asks are the page's to make again: one kept past a reload
            // could no longer be answered.
            let history: Vec<_> = self
                .notifications
                .log()
                .history()
                .iter()
                .filter(|notification| !tilo_desktop_surface::is_host_offer(notification))
                .cloned()
                .collect();
            let bytes = tilo_desktop_surface::notifications::encode_history(
                &history,
                ui::time::SystemTime::now(),
                ui::time::now(),
            )
            .map_err(|error| error.to_string())?;
            self.store
                .write(&self.notifications_key, WriteOp::Replace(bytes), None)
                .map_err(|error| error.to_string())?;
            self.saved_notifications_revision
                .set(notifications_revision);
        }
        let revision = handle.revision();
        if revision == self.saved_revision.get()
            && !self.records_changed.get()
            && !self.history_dirty.get()
        {
            return Ok(());
        }
        let stored = stored_desktop(
            handle,
            &self.records.borrow(),
            &self.unbound.borrow(),
            &self.authors.borrow(),
        );
        let value = serde_json::to_value(&stored).map_err(|error| error.to_string())?;
        let bytes = serde_json::to_vec(&value).map_err(|error| error.to_string())?;
        self.store
            .write(&self.key, WriteOp::Replace(bytes), None)
            .map_err(|error| error.to_string())?;
        let mut history = self.history.borrow_mut();
        if tilo_desktop_surface::history_log::record(
            &mut history,
            &value,
            &self.author,
            ui::time::unix_now_i64(),
        ) {
            self.history_dirty.set(true);
        }
        tilo_desktop_surface::history_log::save_pending(
            &mut history,
            &self.history_dirty,
            |bytes| {
                self.store
                    .write(&self.history_key, WriteOp::Replace(bytes), None)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            },
        )?;
        self.saved_revision.set(revision);
        self.records_changed.set(false);
        Ok(())
    }
}

fn stored_desktop(
    handle: &DesktopHandle,
    bindings: &HashMap<TabId, RecordCheckout>,
    unbound: &HashMap<TabId, String>,
    authors: &BTreeMap<u64, String>,
) -> StoredDesktop {
    handle.read(|tree| {
        let mut live = std::collections::HashSet::new();
        let mut strips = std::collections::HashSet::new();
        for workspace in &tree.workspaces {
            for strip in &workspace.strips {
                strips.insert(strip.id.0);
                for group in &strip.window_groups {
                    live.extend(group.tabs.iter().map(|tab| tab.id));
                }
            }
        }
        let mut records: Vec<(TabId, String)> = bindings
            .iter()
            .filter(|(tab, _)| live.contains(tab))
            .map(|(tab, checkout)| (*tab, checkout.id.as_str().to_owned()))
            .chain(
                unbound
                    .iter()
                    .filter(|(tab, _)| live.contains(tab) && !bindings.contains_key(tab))
                    .map(|(tab, record)| (*tab, record.clone())),
            )
            .collect();
        records.compact_sort();
        let authors = authors
            .iter()
            .filter(|(id, _)| strips.contains(*id) || live.contains(*id))
            .map(|(id, author)| (*id, author.clone()))
            .collect();
        StoredDesktop {
            tree: tree.clone(),
            records,
            authors,
        }
    })
}

/// The desktop as peers share it: the tree and each tab's record.
pub(crate) fn shared_value(handle: &DesktopHandle, shared: &Shared) -> serde_json::Value {
    let mut desktop = stored_desktop(
        handle,
        &shared.records.borrow(),
        &shared.unbound.borrow(),
        &shared.authors.borrow(),
    );
    desktop.tree = handle.shared_tree();
    serde_json::to_value(desktop).unwrap_or(serde_json::Value::Null)
}

/// Take a desktop a peer shared: bind its tabs' records, then hand the tree
/// to the surface's reconcile.
pub(crate) fn apply_shared(
    handle: &DesktopHandle,
    shared: &Shared,
    value: &serde_json::Value,
) -> Result<(), String> {
    let desktop: StoredDesktop =
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
    if desktop.tree.workspaces.is_empty() {
        return Err("a desktop needs a workspace".into());
    }
    {
        let (bound, unbound) = stored_records(&desktop);
        shared.unbound.borrow_mut().extend(unbound);
        let mut authors = shared.authors.borrow_mut();
        for (id, author) in &desktop.authors {
            authors.entry(*id).or_insert_with(|| author.clone());
        }
        drop(authors);
        let mut records = shared.records.borrow_mut();
        for (tab, checkout) in bound {
            if records
                .get(&tab)
                .is_none_or(|bound| bound.id != checkout.id)
            {
                records.insert(tab, checkout);
                shared.records_changed.set(true);
            }
        }
    }
    let local_tabs: Vec<_> = shared.peer_windows.borrow().keys().copied().collect();
    handle.replace_shared_tree(desktop.tree, &local_tabs);
    shared.wake();
    Ok(())
}

/// Put the room's desktop `value` back over a local change its person may
/// not make, attribution included: what the room says made each strip and
/// window is what this desktop says too.
///
/// # Errors
/// As [`apply_shared`].
pub(crate) fn return_to(
    handle: &DesktopHandle,
    shared: &Shared,
    value: &serde_json::Value,
) -> Result<(), String> {
    let desktop: StoredDesktop =
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
    {
        let mut authors = shared.authors.borrow_mut();
        let live: std::collections::HashSet<u64> = desktop
            .tree
            .workspaces
            .iter()
            .flat_map(|w| &w.strips)
            .flat_map(|s| {
                std::iter::once(s.id.0).chain(
                    s.window_groups
                        .iter()
                        .flat_map(|g| g.tabs.iter().map(|t| t.id)),
                )
            })
            .collect();
        authors.retain(|id, _| !live.contains(id) || desktop.authors.contains_key(id));
        for (id, author) in &desktop.authors {
            authors.insert(*id, author.clone());
        }
    }
    apply_shared(handle, shared, value)
}

fn load_stored(store: &dyn DurableStore, key: &StorageKey) -> Option<StoredDesktop> {
    let stored = store.read(key).ok()??;
    let desktop: StoredDesktop = serde_json::from_slice(&stored.bytes).ok()?;
    (!desktop.tree.workspaces.is_empty()).then_some(desktop)
}

/// The record bindings of a stored desktop, for the tabs whose application
/// the page still registers: bound where the application is loaded, as
/// record ids where it is not yet.
fn stored_records(
    desktop: &StoredDesktop,
) -> (HashMap<TabId, RecordCheckout>, HashMap<TabId, String>) {
    let mut apps = HashMap::new();
    for workspace in &desktop.tree.workspaces {
        for strip in &workspace.strips {
            for group in &strip.window_groups {
                for tab in &group.tabs {
                    if let TileContent::LocalApp { app_id, .. } = &tab.tile {
                        apps.insert(tab.id, app_id.clone());
                    }
                }
            }
        }
    }
    let mut bound = HashMap::new();
    let mut unbound = HashMap::new();
    for (tab, record) in &desktop.records {
        let Some(app) = apps.get(tab) else { continue };
        if let Some(definition) = application(app) {
            if let Some(id) = RecordId::parse(record) {
                bound.insert(
                    *tab,
                    RecordCheckout::new(id, definition.record_kind.clone()),
                );
            }
        } else if known_application(app).is_some() {
            unbound.insert(*tab, record.clone());
        }
    }
    (bound, unbound)
}

/// Mount the room's desktop from the profile, or seed one: one workspace
/// named after the room, one strip, a window per requested application
/// (the first registered one by default).
///
/// # Errors
/// Refuses an application id the page does not register.
pub(crate) fn mount(
    mut inputs: Inputs,
    store: Rc<dyn DurableStore>,
    apps: Option<&[String]>,
    workspace_name: Option<&str>,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Result<(DesktopSurface, DesktopHandle, Shared, Persistence), String> {
    let key = StorageKey::record(DESKTOP_PACKAGE, inputs.room.clone(), "desktop");
    let salt = peer_salt(&inputs.salt);
    let mut shared = Shared::default();
    shared.room_actions.peer = inputs.peer;
    if inputs.peer.is_some() {
        // In a room, the person holds what the room answers, once it has.
        shared.standing = Standing::unknown();
    }
    shared.linked = linked_accounts(&shared, &notify);
    let late = LateFeedback {
        author: inputs.author.clone(),
        store: Rc::clone(&store),
        models: inputs.models.clone(),
    };
    if let Some(startup) = inputs.feedback.take() {
        late.install(&shared, startup)?;
    } else if let Some(peer) = inputs.peer {
        // The page's destination is still being admitted: the desktop does
        // not wait for it, and takes it up once it is here.
        crate::feedback_delivery::wake_on_startup(peer, Arc::clone(&notify));
        *shared.late_feedback.borrow_mut() = Some((peer, late));
    }
    let tree = if let Some(stored) = load_stored(store.as_ref(), &key) {
        let (bound, unbound) = stored_records(&stored);
        *shared.records.borrow_mut() = bound;
        *shared.unbound.borrow_mut() = unbound;
        *shared.authors.borrow_mut() = stored.authors;
        stored.tree
    } else {
        let definitions: Vec<(&str, &'static str)> = match apps {
            Some(ids) => ids
                .iter()
                .map(|id| {
                    known_application(id)
                        .map(|title| (id.as_str(), title))
                        .ok_or_else(|| format!("unsupported application {id}"))
                })
                .collect::<Result<_, _>>()?,
            // A fresh desktop opens no window: its empty workspace shows the
            // launchpad, and every registered app is in search.
            None => Vec::new(),
        };
        let mut minter = IdMinter::new(salt);
        let tabs = definitions
            .into_iter()
            .map(|(app, title)| app_tab(minter.mint(), app, title))
            .collect();
        let name = workspace_name
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or("Workspace");
        tilo_desktop_surface::seed_tree(&mut minter, name, tabs)
    };
    let history_key = StorageKey::record(DESKTOP_PACKAGE, inputs.room.clone(), "history");
    let history = store
        .read(&history_key)
        .ok()
        .flatten()
        .and_then(|stored| AutoCommit::load(&stored.bytes).ok())
        .unwrap_or_default();
    let notifications_key =
        StorageKey::record(DESKTOP_PACKAGE, inputs.room.clone(), "notifications");
    let notifications = tilo_desktop_surface::DesktopNotifications::default();
    if let Some(history) = store
        .read(&notifications_key)
        .ok()
        .flatten()
        .and_then(|stored| {
            tilo_desktop_surface::notifications::decode_history(
                &stored.bytes,
                ui::time::SystemTime::now(),
                ui::time::now(),
            )
            .ok()
        })
    {
        notifications.restore_history(history);
    }
    let saved_notifications_revision = Cell::new(notifications.revision());
    let persistence = Persistence {
        store,
        key,
        saved_revision: Cell::new(0),
        records_changed: Rc::clone(&shared.records_changed),
        records: Rc::clone(&shared.records),
        unbound: Rc::clone(&shared.unbound),
        authors: Rc::clone(&shared.authors),
        history: Rc::new(RefCell::new(history)),
        history_key,
        history_dirty: Cell::new(false),
        notifications,
        notifications_key,
        saved_notifications_revision,
        author: inputs.author.clone(),
    };
    let tiles = Tiles {
        inputs,
        shared: shared.clone(),
        stores: HashMap::new(),
        notify,
    };
    let (surface, handle) = DesktopSurface::new(tree, salt, Box::new(tiles));
    Ok((surface, handle, shared, persistence))
}

/// Parse a `DesktopTree` (JSON) changed elsewhere, for
/// [`DesktopHandle::replace_tree`].
pub(crate) fn parse_tree(json: &str) -> Result<DesktopTree, String> {
    let tree = serde_json::from_str(json)
        .and_then(DesktopTree::from_json)
        .map_err(|error| error.to_string())?;
    if tree.workspaces.is_empty() {
        return Err("a desktop needs a workspace".into());
    }
    Ok(tree)
}
