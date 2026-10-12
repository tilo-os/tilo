//! The session desktop in the page: `tilo-desktop-surface` over a
//! `tilo-desktop-model` tree, with every tab an in-process application
//! component registered through its `ApplicationDefinition`.
//!
//! The tree and the record each tab shows are kept per room in the profile's
//! durable store ([`Persistence`]) and committed to `IndexedDB` with the
//! records themselves, so a reload restores the same windows on the same
//! documents. A profile with no stored desktop is seeded with a window per
//! requested application.
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use durable_store::{DurableStore, StorageKey, WriteOp};
use tilo_desktop_model::{DesktopTree, IdMinter, Tab, TabId, TileContent, Workspace};
use tilo_desktop_surface::navigation::Application;
use tilo_desktop_surface::{DesktopHandle, DesktopSurface, TileSource};
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
/// else the built-in Scratch, so a runtime without apps still opens one.
pub(crate) fn applications() -> Vec<&'static ApplicationDefinition> {
    let registered = ui::application::registry::applications();
    let mut applications = if registered.is_empty() {
        vec![crate::scratch::definition()]
    } else {
        registered
    };
    if !applications
        .iter()
        .any(|app| app.app_id == tilo_desktop::feedback::PACKAGE)
    {
        applications.push(tilo_desktop::feedback::definition());
    }
    applications
}

/// An application the page can open by id, Scratch included: windows a
/// desktop saved before keep their application.
pub(crate) fn application(app: &str) -> Option<&'static ApplicationDefinition> {
    applications()
        .into_iter()
        .chain(std::iter::once(crate::scratch::definition()))
        .find(|definition| definition.app_id == app)
}

/// What a group's new-tab control opens: Notes where the bundle has it,
/// else the first application.
const NEW_TAB_APP: &str = "so.tilo.notes";

pub(crate) fn catalogue() -> Vec<Application> {
    applications()
        .into_iter()
        .map(|definition| Application {
            id: definition.app_id.into(),
            label: definition.title.into(),
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
}

/// State the mount and the tile source share.
#[derive(Clone, Default)]
pub(crate) struct Shared {
    /// Trusted host custody, never configured from a launch URL or draft.
    pub feedback: Rc<RefCell<Option<tilo_desktop::feedback::PrivateFeedback>>>,
    /// Every package's record owner opened so far, flushed with the mount.
    pub stores: Rc<RefCell<Vec<RecordStore>>>,
    pub images: Rc<RefCell<Vec<ui::image_store::HostImageLoader>>>,
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
    /// Tabs bound to a record that could not open yet: a peer's desktop
    /// named it before the record itself arrived.
    waiting: Rc<RefCell<std::collections::HashSet<TabId>>>,
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

    pub fn take_wake(&self) -> bool {
        self.woken.swap(false, Ordering::AcqRel)
    }

    /// The applications whose components woke the mount since the last call.
    pub fn take_wakers(&self) -> Vec<&'static str> {
        std::mem::take(
            &mut *self
                .wakers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
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
                .is_none_or(|package| application(package).is_none())
        });
        *self.node_apps.borrow_mut() = apps;
    }

    /// The search entries of the applications the node runs, then of the
    /// catalog's apps this desktop cannot open, the node's left out.
    fn node_search_entries(&self) -> Vec<tilo_desktop_search::providers::AppEntry> {
        let node_apps = self.node_apps.borrow();
        let hosted: Vec<&str> = node_apps
            .iter()
            .filter_map(|node| node.app.as_deref())
            .collect();
        node_apps
            .iter()
            .map(|node| tilo_desktop_search::providers::AppEntry {
                id: format!("{NODE_APP_LAUNCH}{}", node.id),
                title: node.title.clone(),
                category: node.app.as_deref().and_then(|package| {
                    tilo_desktop_search::availability::catalog()
                        .iter()
                        .find(|app| app.id == package && !app.category.is_empty())
                        .map(|app| app.category.into())
                }),
                description: Some(NODE_APP_DESCRIPTION.into()),
                unavailable: None,
            })
            .chain(tilo_desktop_search::availability::unavailable_on(
                &["web"],
                &hosted,
            ))
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

    /// Every tab's bound record.
    pub fn bound_records(&self) -> Vec<(TabId, RecordCheckout)> {
        self.records
            .borrow()
            .iter()
            .map(|(tab, checkout)| (*tab, checkout.clone()))
            .collect()
    }

    /// The record `tab` shows, once its component bound one.
    pub fn record_of(&self, tab: TabId) -> Option<String> {
        self.records
            .borrow()
            .get(&tab)
            .map(|checkout| checkout.id.as_str().to_owned())
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
            .inputs
            .owners
            .open(definition.app_id, &self.inputs.room)
            .map_err(|error| error.to_string())?;
        self.stores.insert(definition.app_id, store.clone());
        self.shared.stores.borrow_mut().push(store.clone());
        Ok(store)
    }

    fn component(&mut self, tab: &Tab) -> Result<AppComponent, String> {
        if let TileContent::PeerWindow { label, .. } = &tab.tile {
            // The page draws the stream over this window's content.
            return Err(if self.shared.peer_windows.borrow().contains_key(&tab.id) {
                format!("Connecting to {label}…")
            } else {
                format!("{label} is no longer shared")
            });
        }
        let TileContent::LocalApp { app_id, .. } = &tab.tile else {
            return Err("This window has no application in the browser".into());
        };
        let definition = application(app_id)
            .ok_or_else(|| format!("{app_id} is not available in the browser"))?;
        if definition.app_id == tilo_desktop::feedback::PACKAGE {
            return self.private_feedback(tab.id);
        }
        let store = self.store(definition)?;
        let saved = self.shared.records.borrow().get(&tab.id).cloned();
        let (target, named_new) = match saved {
            Some(checkout) => (
                LaunchTarget::Record {
                    checkout,
                    reveal: None,
                },
                None,
            ),
            None if definition.keeps_room_record() => (definition.unsaved_target(&store), None),
            None => self.unbound_target(definition, &store, tab.id)?,
        };
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
        let context = match named_new {
            Some(id) => HostContext::prepare_new_record(definition, inputs, id),
            None => HostContext::prepare(definition, inputs, &target),
        }
        .map_err(|error| error.to_string())?;
        let destination = Rc::clone(&self.shared.images);
        let mut component = ui::ImageStore::with_host_factory_with_max_dimension(
            Rc::new(move |max_dimension| {
                let (image, loader) = ui::ImageStore::host_driven_with_max_dimension(max_dimension);
                destination.borrow_mut().push(loader);
                image
            }),
            || definition.instantiate(context, target, restore),
        )
        .map_err(|error| error.to_string())?;
        let woken = Arc::clone(&self.shared.woken);
        let wakers = Arc::clone(&self.shared.wakers);
        let notify = Arc::clone(&self.notify);
        let app_id = definition.app_id;
        component
            .mount_with_owner_queue(&self.shared.owner_wakes, move |_| {
                woken.store(true, Ordering::Release);
                let mut wakers = wakers
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
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

impl Tiles {
    fn private_feedback(&self, instance: TabId) -> Result<AppComponent, String> {
        let mut component = match self.shared.feedback.borrow().as_ref() {
            Some(feedback) => feedback.prepare(&self.inputs.device, &self.inputs.author, instance),
            None => tilo_desktop::feedback::unconfigured(
                &self.inputs.device,
                &self.inputs.author,
                instance,
            ),
        }
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
        if let Some(notice) = crate::crash::quarantined(tab.id, &tab.title) {
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
            Err(detail) => {
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

    fn new_tab(&mut self, _: &Workspace, id: TabId) -> Option<Tab> {
        let definition = application(NEW_TAB_APP).or_else(|| applications().into_iter().next())?;
        Some(app_tab(id, definition))
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
            };
            self.shared.peer_windows.borrow_mut().insert(id, window);
            return Some(tab);
        }
        Some(app_tab(id, application(app)?))
    }

    fn search_applications(&self) -> Vec<tilo_desktop_search::providers::AppEntry> {
        applications()
            .into_iter()
            .map(|definition| tilo_desktop_search::providers::AppEntry {
                id: definition.app_id.into(),
                title: definition.title.into(),
                category: None,
                description: None,
                unavailable: None,
            })
            .chain(self.shared.node_search_entries())
            .collect()
    }
}

fn app_tab(id: TabId, definition: &ApplicationDefinition) -> Tab {
    Tab {
        id,
        title: definition.title.into(),
        tile: TileContent::LocalApp {
            app_id: definition.app_id.into(),
            label: definition.title.into(),
            note: String::new(),
        },
    }
}

/// This device's share of the desktop id space, so ids it mints never
/// collide with another device's once trees merge.
fn peer_salt(device: &str) -> u16 {
    // FNV-1a folded to 16 bits: stable across builds and platforms.
    let mut hash: u32 = 0x811c_9dc5;
    for byte in device.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    let folded = (hash >> 16) ^ (hash & 0xffff);
    u16::try_from(folded).unwrap_or(u16::MAX)
}

/// Where a room's desktop lives in the profile. Changing it orphans stored
/// desktops.
pub(crate) const DESKTOP_PACKAGE: &str = "so.tilo.browser-desktop";

/// The stored form of a room's desktop.
#[derive(serde::Serialize, serde::Deserialize)]
struct StoredDesktop {
    tree: DesktopTree,
    /// `(tab, record id)` for every tab whose component bound a record.
    records: Vec<(TabId, String)>,
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
            let bytes = tilo_desktop_surface::notifications::encode_history(
                self.notifications.log().history(),
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
        let stored = stored_desktop(handle, &self.records.borrow());
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
) -> StoredDesktop {
    handle.read(|tree| {
        let mut live = std::collections::HashSet::new();
        for workspace in &tree.workspaces {
            for strip in &workspace.strips {
                for group in &strip.window_groups {
                    live.extend(group.tabs.iter().map(|tab| tab.id));
                }
            }
        }
        let mut records: Vec<(TabId, String)> = bindings
            .iter()
            .filter(|(tab, _)| live.contains(tab))
            .map(|(tab, checkout)| (*tab, checkout.id.as_str().to_owned()))
            .collect();
        records.sort_unstable();
        StoredDesktop {
            tree: tree.clone(),
            records,
        }
    })
}

/// The desktop as peers share it: the tree and each tab's record.
pub(crate) fn shared_value(handle: &DesktopHandle, shared: &Shared) -> serde_json::Value {
    let mut desktop = stored_desktop(handle, &shared.records.borrow());
    desktop.tree = handle.shared_tree();
    serde_json::to_value(desktop).unwrap_or(serde_json::Value::Null)
}

/// Take a desktop a peer shared: bind its tabs' records, then hand the tree
/// to the surface's reconcile.
///
/// # Errors
/// The value is not a shared desktop, or it has no workspace.
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
        let mut records = shared.records.borrow_mut();
        for (tab, checkout) in stored_records(&desktop) {
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

fn load_stored(store: &dyn DurableStore, key: &StorageKey) -> Option<StoredDesktop> {
    let stored = store.read(key).ok()??;
    let desktop: StoredDesktop = serde_json::from_slice(&stored.bytes).ok()?;
    (!desktop.tree.workspaces.is_empty()).then_some(desktop)
}

/// The record bindings of a stored desktop, for the tabs whose application
/// the page still registers.
fn stored_records(desktop: &StoredDesktop) -> HashMap<TabId, RecordCheckout> {
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
    desktop
        .records
        .iter()
        .filter_map(|(tab, record)| {
            let definition = application(apps.get(tab)?)?;
            let id = RecordId::parse(record)?;
            Some((
                *tab,
                RecordCheckout::new(id, definition.record_kind.clone()),
            ))
        })
        .collect()
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
    let shared = Shared::default();
    if let Some(startup) = inputs.feedback.take() {
        if startup.reporter().principal()
            != &tilo_actions::remote::CallerPrincipal::Person(inputs.author.clone())
        {
            return Err("feedback reporter does not own this profile".into());
        }
        startup
            .install(Rc::clone(&store), &mut shared.feedback.borrow_mut())
            .map_err(|error| error.to_string())?;
    }
    let tree = if let Some(stored) = load_stored(store.as_ref(), &key) {
        *shared.records.borrow_mut() = stored_records(&stored);
        stored.tree
    } else {
        let definitions: Vec<&'static ApplicationDefinition> = match apps {
            Some(ids) => ids
                .iter()
                .map(|id| application(id).ok_or_else(|| format!("unsupported application {id}")))
                .collect::<Result<_, _>>()?,
            // A fresh desktop opens no window: its empty workspace shows the
            // launchpad, and every registered app is in search.
            None => Vec::new(),
        };
        let mut minter = IdMinter::new(salt);
        let tabs = definitions
            .into_iter()
            .map(|definition| app_tab(minter.mint(), definition))
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
///
/// # Errors
/// The JSON is not a desktop tree, or it has no workspace.
pub(crate) fn parse_tree(json: &str) -> Result<DesktopTree, String> {
    let tree: DesktopTree = serde_json::from_str(json).map_err(|error| error.to_string())?;
    if tree.workspaces.is_empty() {
        return Err("a desktop needs a workspace".into());
    }
    Ok(tree)
}
