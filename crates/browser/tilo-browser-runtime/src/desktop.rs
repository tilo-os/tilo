//! The session desktop in the page: `tilo-desktop-surface` over a
//! `tilo-desktop-model` tree, with every tab an in-process application
//! component registered through its `ApplicationDefinition`.
//!
//! The tree and the record each tab shows are kept per workspace in the profile's
//! durable store ([`Persistence`]) and committed to `IndexedDB` with the
//! records themselves, so a reload restores the same windows on the same
//! documents. A profile with no stored desktop is seeded with a window per
//! requested application.
pub(crate) type PersonIdentity = (String, (u8, u8, u8));

use durable_store::{DurableStore, StorageKey, WriteOp};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tilo_actions::InvocationId;
use tilo_app::linked_accounts::ActionAnswer;
use tilo_desktop_model::{IdMinter, Layout, Tab, TabId, TileContent, Workspace};
use tilo_desktop_surface::navigation::Application;
use tilo_desktop_surface::{DesktopHandle, DesktopSurface, TileSource};
use tilo_lock::MutexRecover as _;
use tilo_session_actions::SessionActions;
use tilo_session_core::layout::{LayoutDocument, Store};
use ui::application::{
    AppComponent, ApplicationDefinition, LaunchTarget, ProcessContext, ProcessContextInputs,
    RestoreInput,
};
use ui::application_view::InstanceViewStore;
use ui::automerge::AutoCommit;
use ui::permissions::{MemberId, SharedPermissions};
use ui::resource::{DocumentCheckout, DocumentId, ResourceOwners, ResourceStore};
use ui::widget::TileHost;
use ui::window_share::ResourceView;

use crate::app_page::Unbuilt;
use crate::shell::{HostRequest, HostRequests};

/// Applications the page can open, in launcher order: every application
/// the bundle registered from its manifests (`ui::application::registry`).
/// A bundle whose apps are all still deferred has apps, just none loaded
/// yet.
pub(crate) fn applications() -> Vec<&'static ApplicationDefinition> {
    let mut applications = ui::application::registry::applications();
    // The composer is an application of the bundle when it registered it
    // (fetched on first open); otherwise it is linked in here.
    if !applications
        .iter()
        .any(|app| app.app_id() == tilo_desktop::feedback::PACKAGE)
        && let Some(code) = crate::feedback_composer::code()
    {
        applications.push((code.definition)());
    }
    applications
}

/// An application the page can open by id whose code is loaded: windows a
/// desktop saved before keep their application.
pub(crate) fn application(app: &str) -> Option<&'static ApplicationDefinition> {
    applications()
        .into_iter()
        .find(|definition| definition.app_id() == app)
}

/// Every application the page can open, as `(id, title)` in launcher
/// order: those loaded ([`applications`]) and those whose code is fetched
/// when they first open (`ui::application::registry::defer`).
pub(crate) fn known_applications() -> Vec<(&'static str, &'static str)> {
    let mut known = ui::application::registry::catalogue();
    for definition in applications() {
        if !known.iter().any(|(id, _)| *id == definition.app_id()) {
            known.push((definition.app_id(), definition.title()));
        }
    }
    known
}

/// The title of an application the page can open by id, loaded or not.
/// Whether `permissions` let this person open `app` here, as the workspace
/// broker decides it on a native desktop: an installed app, and leave to
/// change the app's part of the workspace, or, to show something that
/// exists (`existing`: a record, an item of one, such as a window shared
/// with them), leave to read it. The window it opens is read-only to a
/// person who may only read: the workspace keeps none of their changes.
///
/// # Errors
/// An app this desktop doesn't have, or one this person may not open.
pub(crate) fn may_open(
    permissions: &ui::permissions::PermissionContext,
    app: &str,
    existing: bool,
) -> Result<(), tilo_actions::remote::ActionCallError> {
    known_application(app).ok_or(tilo_actions::remote::ActionCallError::UnknownAction)?;
    let kind = if existing {
        ui::permissions::ActionKind::Read
    } else {
        ui::permissions::ActionKind::Undoable
    };
    if !permissions.check(&ui::permissions::PermissionScope::app(app, kind)) {
        return Err(tilo_actions::remote::ActionCallError::NotPermitted);
    }
    Ok(())
}

/// What a window of `app`, which this page does not run, shows instead:
/// where the app runs.
fn not_in_the_browser(app: &str) -> String {
    tilo_desktop_search::availability::runs_elsewhere(app, "the browser")
}

pub(crate) fn known_application(app: &str) -> Option<&'static str> {
    known_applications()
        .into_iter()
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
    pub feedback: Option<tilo_feedback_transport::Startup>,
    pub owners: ResourceOwners,
    /// The store [`Self::owners`] keep records in: the profile's, where the
    /// person's own records are kept too ([`crate::person_resources`]).
    pub store: Rc<dyn durable_store::DurableStore>,
    pub views: Rc<InstanceViewStore>,
    pub device: String,
    /// Seeds this desktop's share of the id space: the certified device
    /// key when the mount has a peer (every browser's profile namespace may
    /// be the same string), else the device namespace.
    pub salt: String,
    pub workspace: String,
    pub author: String,
    pub permissions: SharedPermissions,
    pub models: ui::models::Models,
    /// What a report's review calls [`Self::models`]; empty when they are
    /// the page's own model service rather than its host's.
    pub model_name: String,
    pub provider_admin: ui::models::ProviderAdministration,
    /// The peer whose workspace this desktop shows, when it has one.
    pub peer: Option<u32>,
    /// The content store the desktop lends every window it opens.
    pub content: tilo_content::ContentStore,
}

/// The person who made each strip or opened each window, by its id. Kept
/// with the desktop: who may close, move or rename each one
/// ([`tilo_desktop_model::ownership`]).
pub(crate) type Authors = Rc<RefCell<BTreeMap<u64, String>>>;

/// The roles a workspace answers a page's person holds, in the order
/// [`SharedRole`] counts them.
const ROLES: [tilo_action_policy::Role; 5] = [
    tilo_action_policy::Role::Viewer,
    tilo_action_policy::Role::Commenter,
    tilo_action_policy::Role::Editor,
    tilo_action_policy::Role::Moderator,
    tilo_action_policy::Role::Owner,
];

/// The role this desktop's person holds where it is: the owner of a desktop
/// of their own, and in a workspace what the workspace last answered (not known
/// until it has). Shared with the workspace's replication, which asks.
#[derive(Clone, Debug)]
pub(crate) struct SharedRole(Arc<std::sync::atomic::AtomicU8>);

impl Default for SharedRole {
    /// A desktop of the person's own: theirs to arrange.
    fn default() -> Self {
        let role = Self::unknown();
        role.set(Some(tilo_action_policy::Role::Owner));
        role
    }
}

impl SharedRole {
    /// A role not known yet.
    pub fn unknown() -> Self {
        Self(Arc::default())
    }

    /// The role, as [`ROLES`] position plus one; 0 while not known.
    pub fn code(&self) -> u8 {
        self.0.load(Ordering::Relaxed)
    }

    pub fn role(&self) -> Option<tilo_action_policy::Role> {
        self.code()
            .checked_sub(1)
            .and_then(|at| ROLES.get(usize::from(at)).copied())
    }

    /// Hold `role` (`None`: not known); whether that changed it.
    pub fn set(&self, role: Option<tilo_action_policy::Role>) -> bool {
        let code = role
            .and_then(|role| ROLES.iter().position(|held| *held == role))
            .and_then(|at| u8::try_from(at).ok())
            .map_or(0, |at| at.saturating_add(1));
        self.0.swap(code, Ordering::Relaxed) != code
    }

    /// The same member access as `other` from now on: one answer reaches both.
    pub fn share(&mut self, other: &Self) {
        self.0 = Arc::clone(&other.0);
    }

    /// `person` as the desktop's policy asks about them.
    pub fn of<'a>(&self, person: &'a str) -> tilo_desktop_model::commands::Requester<'a> {
        tilo_desktop_model::commands::Requester {
            person,
            role: self.role(),
            members: None,
        }
    }
}

/// What a desktop in a workspace installs its feedback startup with.
pub(crate) struct LateFeedback {
    peer: u32,
    /// Asks the mount for a frame, which takes a registered startup up.
    wake: Arc<dyn Fn() + Send + Sync>,
    /// Admissions asked for again after one went stale before it was
    /// installed.
    tries: u8,
    author: String,
    store: Rc<dyn DurableStore>,
    models: ui::models::Models,
    model_name: String,
}

impl LateFeedback {
    fn install(
        &self,
        shared: &Shared,
        startup: tilo_feedback_transport::Startup,
    ) -> Result<(), String> {
        if startup.reporter().acting().person_id() != Some(self.author.as_str()) {
            return Err("feedback reporter does not own this profile".into());
        }
        if shared.feedback.borrow().is_some() {
            return Err("feedback is already installed".into());
        }
        let sender = crate::feedback_code::install(startup, Rc::clone(&self.store))?;
        *shared.feedback.borrow_mut() = Some(sender);
        // Reports are looked over before they are sent: by the model the
        // embedder lends this page, else the one the reports' owner
        // lends its reporters, else the person's own model on their server.
        let lent = if self.model_name.is_empty() {
            crate::feedback_code::owner_model(self.peer).or_else(|| {
                self.models
                    .is_available()
                    .then(|| (self.models.clone(), "your own model".to_owned()))
            })
        } else {
            Some((self.models.clone(), self.model_name.clone()))
        };
        if let (Some(feedback), Some((models, name))) = (shared.feedback.borrow().as_ref(), lent) {
            feedback.set_models(models, name);
        }
        Ok(())
    }
}

impl Shared {
    /// Install the feedback startup the page registered after the desktop
    /// mounted; whether it did just now.
    pub(crate) fn adopt_feedback(&self) -> bool {
        let Some(peer) = self.late_feedback.borrow().as_ref().map(|late| late.peer) else {
            return false;
        };
        let Some(startup) = crate::feedback_code::take_startup(peer) else {
            return false;
        };
        let Some(late) = self.late_feedback.borrow_mut().take() else {
            return false;
        };
        self.settle_feedback(late, startup)
    }

    /// Take the page's feedback destination up once it is admitted.
    fn await_feedback(&self, late: LateFeedback) {
        crate::feedback_code::wake_on_startup(late.peer, Arc::clone(&late.wake));
        *self.late_feedback.borrow_mut() = Some(late);
    }

    /// Install `startup`; whether it is installed. Installing checks the
    /// owner's admission is fresh, and one that went stale while the page
    /// was busy (a slow first frame) is asked for again rather than leaving
    /// the desktop without reports.
    fn settle_feedback(
        &self,
        mut late: LateFeedback,
        startup: tilo_feedback_transport::Startup,
    ) -> bool {
        let Err(error) = late.install(self, startup) else {
            return true;
        };
        if late.tries < 3 && crate::feedback_code::readmit(late.peer) {
            late.tries = late.tries.saturating_add(1);
            self.await_feedback(late);
        } else {
            web_sys::console::warn_1(&format!("feedback: {error}").into());
        }
        false
    }
}

/// State the mount and the tile source share.
#[derive(Clone, Default)]
pub(crate) struct Shared {
    /// Trusted host ownership, never configured from a launch URL or draft.
    pub feedback: Rc<RefCell<Option<Box<dyn crate::feedback_code::FeedbackSender>>>>,
    /// What installs feedback once the page holds a destination, when the
    /// desktop mounted before it did ([`Shared::adopt_feedback`]).
    late_feedback: Rc<RefCell<Option<LateFeedback>>>,
    /// Every package's record owner opened so far, flushed with the mount.
    pub stores: Rc<RefCell<Vec<ResourceStore>>>,
    pub images: Rc<RefCell<Vec<ui::image_store::HostImageLoader>>>,
    /// The window each image loader draws for, by its place in `images`
    /// ([`crate::page_content::ImageOwners`]).
    pub image_owners: crate::page_content::ImageOwners,
    /// Set by any tab component's wake; the host relayouts before drawing.
    pub woken: Arc<AtomicBool>,
    /// The page has put a frame on screen while it was visible: the person
    /// can see the desktop. What waits for them to have seen it a while
    /// (the first control hint) counts from here, not from the mount.
    pub presented: Rc<Cell<bool>>,
    pub owner_wakes: ui::owner_wake::OwnerWakeQueue,
    /// The applications whose components woke the mount since it last
    /// looked, for the frame metrics.
    wakers: Arc<std::sync::Mutex<Vec<&'static str>>>,
    /// The record each tab shows, once its component created or opened it.
    records: Rc<RefCell<HashMap<TabId, DocumentCheckout>>>,
    /// A tab bound a record since the desktop was last saved.
    records_changed: Rc<Cell<bool>>,
    /// Tabs that could not open yet: a peer's desktop named a record
    /// before the record itself arrived, or the tab's application is still
    /// loading.
    waiting: Rc<RefCell<std::collections::HashSet<TabId>>>,
    /// Why the workspace cannot answer what windows wait for now, as the page's link to it says
    /// ([`tilo_session_client::link::PeerStatus::unanswerable`]): a window whose resource is on its
    /// way shows that failure instead of waiting for an answer that cannot come, and waits again
    /// once the workspace can answer.
    unanswerable: Rc<RefCell<Option<String>>>,
    /// The tabs whose window could not open what it names, with what the window says instead
    /// ([`crate::window_states`]).
    pub(crate) unopened: Rc<RefCell<HashMap<TabId, String>>>,
    /// The tabs whose app runs on its record in their window.
    landed: Rc<RefCell<std::collections::HashSet<TabId>>>,
    /// The record ids of tabs this page has not bound yet: an application
    /// whose code has not loaded, one it does not run, or a record it does not
    /// read. Each is written back to the shared shell under the spelling it
    /// arrived in, as native keeps the record a window held
    /// ([`tilo_desktop_surface::host::records::adopted`]).
    unbound: Rc<RefCell<HashMap<TabId, String>>>,
    /// The content, by hash, each tab opened with its app is to open: what
    /// another window handed over (a capture, a recording).
    opened_content: Rc<RefCell<HashMap<TabId, String>>>,
    /// The names of files the page asked windows to open, by their content's hash: a file
    /// dropped on the shell opens under the name it had, not as its hash.
    pub content_names: Rc<RefCell<HashMap<String, String>>>,
    /// Who made each strip and opened each window, by strip or tab id.
    pub authors: Authors,
    /// The agent run that opened each window an agent opened, by tab, as the
    /// workspace's shell last said.
    pub opened_by: Rc<RefCell<BTreeMap<TabId, tilo_desktop_model::ownership::OpenedBy>>>,
    /// The governance's revision and the shell's when the page last projected the governance onto
    /// it: a projection is due when either moved.
    pub projected_at: Rc<std::cell::Cell<Option<(u64, u64)>>>,
    /// Whether the workspace lets this desktop's person close and move what
    /// others made, as it last answered ([`crate::replication`] asks).
    pub moderates: Arc<AtomicBool>,
    /// The role the workspace last answered this desktop's person holds, which
    /// the desktop's policy decides what to offer by
    /// ([`tilo_desktop_model::commands::policy`]).
    pub role: SharedRole,
    /// Whether the workspace's host lets this page's person invite others
    /// here, as the host last answered: the bar keeps its invite button and
    /// the sheet offers making a link only where it does
    /// ([`crate::replication`] asks it with the rest of their role, as
    /// the shell asks its own host).
    pub invitable: Arc<AtomicBool>,
    /// Whether the workspace lets this page's person make administrators,
    /// as its host last answered: the invite sheet offers an administrator's
    /// link only where it does.
    pub administers: Arc<AtomicBool>,
    /// The workspace's members as its governance lists them, once heard: what
    /// someone no longer among them made is released, anyone's here to
    /// close ([`tilo_desktop_model::ownership::released`]).
    pub members: Rc<RefCell<Option<std::collections::BTreeSet<String>>>>,
    /// What the workspace's governance calls each of its people, by their person (root key,
    /// hex): whoever is not here now is named by it, never by their key.
    pub names: Rc<RefCell<std::collections::BTreeMap<String, String>>>,
    /// The person's accounts with the services this workspace is connected to,
    /// read from the connectors' workspace records by the workspace's host
    /// ([`RecordActions`]).
    pub linked: tilo_app::linked_accounts::LinkedAccounts,
    /// The workspace's record actions, run by the host this page's session
    /// is with.
    pub resource_actions: RecordActions,
    /// What the desktop asks of the page: the requests its host reads each
    /// frame (`crate::shell::HostRequests`).
    pub requests: HostRequests,
    /// Applications whose code is being fetched.
    loading: Rc<RefCell<crate::app_load::Loads>>,
    /// Applications whose code failed to load since their tabs last asked.
    failed: Rc<RefCell<std::collections::HashSet<String>>>,
    /// An application finished loading (or failed to) since the mount last
    /// looked: its waiting tabs are built again.
    arrived: Rc<Cell<bool>>,
    /// Actions of apps the person chose in the desktop's own chrome (a
    /// launchpad card), for the desktop's session to deliver as theirs.
    app_actions: Rc<RefCell<Vec<tilo_desktop::launchpad::LaunchpadAppAction>>>,
    /// The peer whose workspace this desktop shows, when it has one: what it asks its session for
    /// the pages of the windows other devices host.
    pub peer: Option<u32>,
    /// The workspace it shows: whose live session those windows' pages travel.
    pub workspace: String,
    /// The workspace's agents, as Chat's agents resource names each: an agent is a member of
    /// the workspace like a person is, so a page names it to its windows whether or not a
    /// device of its works here now. `None` until the code that reads them is in and the
    /// workspace delivered them: the page asks again each frame (`Mount::place_agents`).
    pub agents: Option<Rc<dyn crate::workspace_resources::Agents>>,
    /// The owners the workspace's documents open through, by the app that keeps each.
    pub owners: Option<ResourceOwners>,
}

impl Shared {
    /// The record the window of `tab` shows, once its component opened one.
    pub fn resource_of(&self, tab: TabId) -> Option<DocumentCheckout> {
        self.records.borrow().get(&tab).cloned()
    }

    /// `person` as the desktop's policy asks about them here, among the
    /// workspace's `members` (borrowed from [`Self::members`]).
    pub fn requester_of<'a>(
        &self,
        person: &'a str,
        members: Option<&'a std::collections::BTreeSet<String>>,
    ) -> tilo_desktop_model::commands::Requester<'a> {
        tilo_desktop_model::commands::Requester {
            members,
            ..self.role.of(person)
        }
    }

    /// Ask the host to run the surface's update pass, and for the frame it runs in (queued
    /// navigation, a replaced tree, an app action chosen in the bar's cards). A page draws only
    /// when something changes: a flag alone waited for whatever drew next, so the card a press on
    /// an agent's "Open card" asked Console for could stay unopened on a page with nothing else
    /// to draw.
    pub fn wake(&self) {
        self.woken.store(true, Ordering::Release);
        crate::browser::wake_desktops();
    }

    /// The workspace cannot answer this page now, for `cause`, or can again (`None`): the windows
    /// waiting for a resource are built again, showing the failure or waiting once more.
    pub fn follow_unanswerable(&self, cause: Option<String>, handle: &DesktopHandle) {
        if *self.unanswerable.borrow() == cause {
            return;
        }
        *self.unanswerable.borrow_mut() = cause;
        self.retry_waiting(handle);
    }

    /// What a window whose resource is still on its way from the workspace shows: its arrival, or,
    /// while the workspace cannot answer, why ([`Self::follow_unanswerable`]).
    fn awaiting_resource(&self) -> Unready {
        match self.unanswerable.borrow().clone() {
            Some(cause) => Unready::Failed(cause),
            None => Unready::Arriving,
        }
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

    /// Whether `resource` is narrower than `app`'s record `record` (by
    /// route): something a window on that record shows by invoking its
    /// `open`, not the record itself.
    pub fn narrower(app: &str, record: &str, resource: &str) -> bool {
        application(app).is_some_and(|definition| {
            !resource.is_empty()
                && resource != record
                && resource != definition.resource_type.as_str()
        })
    }

    /// The tab showing `app`'s record `record`, by route, among the windows the shell has now,
    /// in the order it holds them. The empty route is the app's own record here: the
    /// workspace's, or the person's own of an app whose manifest says each member keeps their
    /// own (the only one this page opens is its person's). A window that closed keeps its
    /// record bound for a reopen, and is no window to go to or to run an action in.
    pub fn tab_on(&self, handle: &DesktopHandle, app: &str, record: &str) -> Option<TabId> {
        let definition = application(app)?;
        let kind = definition.resource_type.clone();
        let own = definition.member_resource;
        let shows = |route: &str| route == record || (record.is_empty() && own);
        let records = self.records.borrow();
        handle.read(|tree| {
            tree.tabs().map(|tab| tab.id).find(|tab| {
                records
                    .get(tab)
                    .is_some_and(|checkout| shows(checkout.id.route()) && checkout.kind == kind)
            })
        })
    }

    /// The window that already shows `app`'s record `record` (by route, empty for the app's
    /// own): where an open of it goes, in place of a second window on what one shows. The
    /// window whose record this page bound is that window ([`Self::tab_on`]), the one an
    /// open's action then runs in. Before a window's record is bound (its bytes still on
    /// their way), any window of an app that keeps one record, the workspace's or the
    /// person's own, is on that record: Settings and Mail show one thing however many windows
    /// they have. An app whose windows each keep a document of their own has no window on a
    /// record none of them named, so an open that names none opens a window.
    pub fn window_on(&self, handle: &DesktopHandle, app: &str, record: &str) -> Option<TabId> {
        if let Some(tab) = self.tab_on(handle, app, record) {
            return Some(tab);
        }
        let definition = application(app)?;
        if !record.is_empty()
            || !(definition.member_resource || definition.keeps_workspace_resource())
        {
            return None;
        }
        handle
            .read(|tree| tilo_desktop_surface::navigation::window_of_app(tree, app))
            .map(|ids| ids.tab)
    }

    /// Where each window of the desktop stands ([`crate::window_states`]), by its tab in hex as
    /// the page's status names tabs.
    pub(crate) fn window_states(
        &self,
        handle: &DesktopHandle,
    ) -> serde_json::Map<String, serde_json::Value> {
        let drawn = handle.drawn();
        let unopened = self.unopened.borrow();
        let landed = self.landed.borrow();
        handle.read(|tree| {
            tree.tabs()
                .map(|tab| {
                    let state = crate::window_states::WindowState::of(
                        unopened.get(&tab.id).map(String::as_str),
                        landed.contains(&tab.id),
                        drawn.contains(&tab.id),
                    );
                    (format!("{:x}", tab.id), window_state_json(&state))
                })
                .collect()
        })
    }

    /// The applications the desktop's windows show, each once, in the tree's
    /// order: what the page may fetch ahead of a window opened on it later
    /// (the page's `prefetchApplications`), rather than every app the
    /// desktop offers.
    pub(crate) fn open_apps(handle: &DesktopHandle) -> Vec<String> {
        handle.read(|tree| {
            let mut apps: Vec<String> = Vec::new();
            for tab in tree.tabs() {
                let Some(app) = tilo_desktop_model::apps::canonical_app_id(&tab.tile) else {
                    continue;
                };
                if !apps.iter().any(|shown| shown == app.as_str()) {
                    apps.push(app.as_str().to_owned());
                }
            }
            apps
        })
    }

    /// The applications whose code the page is fetching now: a window of one
    /// fills in, and offers its views, once it arrives.
    pub(crate) fn loading_apps(&self) -> Vec<&'static str> {
        let mut apps: Vec<_> = self.loading.borrow().in_flight().collect();
        apps.sort_unstable();
        apps
    }

    /// Code the page fetched is in: the windows waiting for it are built again.
    pub(crate) fn code_arrived(&self) {
        self.arrived.set(true);
        self.wake();
    }

    /// The stores the page keeps of the workspace, opened as `author`.
    pub(crate) fn page_stores(
        &self,
        author: &str,
    ) -> Option<crate::workspace_resources::PageStores> {
        Some(crate::workspace_resources::PageStores {
            owners: self.owners.clone()?,
            stores: Rc::clone(&self.stores),
            workspace: self.workspace.clone(),
            author: author.to_owned(),
            replicated: self.resource_actions.peer.is_some(),
        })
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
        let mut fetch = Box::pin(async move {
            let loaded = ui::application::registry::load(id).await.is_some();
            // Code that arrives after the bound is registered all the same,
            // so opening the window again shows the app at once.
            if shared.loading.borrow_mut().settle(id, attempt) {
                shared.settle_load(id, loaded, &fetched);
            }
        });
        // Polled once here, the page's loader starts fetching the module in
        // this call: a spawned task first runs after the frame that asked
        // for the app, so its fetch waited behind that frame's layout and
        // paint. The task takes the fetch over from there.
        let mut asked = std::task::Context::from_waker(std::task::Waker::noop());
        if fetch.as_mut().poll(&mut asked).is_pending() {
            wasm_bindgen_futures::spawn_local(fetch);
        }
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
        self.code_arrived();
        notify();
    }

    /// The workspace's store of `definition`'s records: the one a window of it
    /// opened, or opened now and flushed and replicated with the mount's.
    fn workspace_store(
        &self,
        owners: &ResourceOwners,
        workspace: &str,
        definition: &'static ApplicationDefinition,
    ) -> Result<ResourceStore, String> {
        let home = (definition.app_id().to_owned(), workspace.to_owned());
        if let Some(store) = self
            .stores
            .borrow()
            .iter()
            .find(|store| store.app_and_home() == home)
        {
            return Ok(store.clone());
        }
        // Clones share the owner registry: the same writer either way.
        let store = owners
            .clone()
            .open(definition.app_id(), workspace)
            .map_err(|error| error.to_string())?;
        // A record not here is the workspace's to send or to say it has none
        // of, never a fresh one of this page's.
        if self.resource_actions.peer.is_some() {
            store.ask_workspace_for_missing();
        }
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

    /// Have the desktop's session deliver `action` to its app as the
    /// person's, opening the app first, on the next frame.
    pub fn ask_app_action(&self, action: tilo_desktop::launchpad::LaunchpadAppAction) {
        self.app_actions.borrow_mut().push(action);
        self.wake();
    }

    /// The app actions asked since the last call.
    pub fn take_app_actions(&self) -> Vec<tilo_desktop::launchpad::LaunchpadAppAction> {
        std::mem::take(&mut *self.app_actions.borrow_mut())
    }

    /// Who holds each window of `tree` this desktop's person `me` may not
    /// close or move, named as `person` (by user root) names people here:
    /// the windows the desktop's policy keeps for whoever opened them
    /// ([`tilo_desktop_model::commands::policy::may_rearrange_window`]),
    /// everyone's but their own unless they moderate the workspace. A
    /// window nobody opened is the workspace's, named `workspace`.
    pub fn holders(
        &self,
        tree: &Layout,
        me: &str,
        workspace: &str,
        person: &mut dyn FnMut(&str) -> PersonIdentity,
    ) -> HashMap<TabId, ui::widget::TabHolder> {
        if self.moderates.load(Ordering::Relaxed) {
            return HashMap::new();
        }
        let members = self.members.borrow();
        let requester = self.requester_of(me, members.as_ref());
        let authors = self.authors.borrow();
        tree.workspaces
            .iter()
            .flat_map(|w| &w.strips)
            .flat_map(|s| &s.window_groups)
            .flat_map(|g| &g.tabs)
            .filter(|tab| {
                tilo_desktop_model::commands::policy::may_rearrange_window(
                    &requester, &authors, tab.id,
                )
                .is_err()
            })
            .map(|tab| {
                let (name, rgb) = match authors.get(&tab.id) {
                    Some(author) => person(author),
                    None => (workspace.to_owned(), ui::presence::user_color(workspace)),
                };
                (tab.id, ui::widget::TabHolder { name, rgb })
            })
            .collect()
    }

    /// The record each window of `tree` shows, in the spelling links and the shared shell use
    /// (empty for the app's primary record): bound, or still waiting for the record a link or a
    /// window named.
    pub fn window_resources(&self, tree: &Layout) -> Vec<(TabId, String)> {
        tilo_desktop_surface::host::records::live(
            &self.records.borrow(),
            &self.unbound.borrow(),
            tree,
        )
    }

    pub fn bound_records(&self) -> Vec<(TabId, DocumentCheckout)> {
        self.records
            .borrow()
            .iter()
            .map(|(tab, checkout)| (*tab, checkout.clone()))
            .collect()
    }

    /// The record `tab` shows in this desktop's workspace, as a report about
    /// its window names it: what a recording of the window records.
    pub fn shown_record(
        &self,
        tree: &Layout,
        tab: TabId,
    ) -> Option<tilo_desktop::feedback::ShownResource> {
        if self.window_capture_protected(tree, tab) {
            return None;
        }
        let workspace = crate::replicated::workspace_uuid(self.resource_actions.peer?)?;
        let (_, record) = self.live_record_of(tree, tab)?;
        Some(tilo_desktop::feedback::ShownResource { workspace, record })
    }

    /// Whether the app window `tab` shows declares itself withheld from what
    /// leaves the device.
    pub fn window_capture_protected(&self, tree: &Layout, tab: TabId) -> bool {
        self.live_record_of(tree, tab)
            .is_some_and(|(app, _)| crate::capture_declarations::declared(&app))
    }

    /// The record `tab` shows as the workspace's live layer names it: its
    /// application and the record id, empty for the package's primary
    /// record.
    pub fn live_record_of(&self, tree: &Layout, tab: TabId) -> Option<(String, String)> {
        tilo_desktop_surface::host::records::of_window(tree, &self.records.borrow(), tab)
    }

    /// The record each window shows, by its tab: what the session's roster places a person's
    /// record by, as a native session's windows note theirs. An app's primary record is spelled
    /// empty, as an agent working in it names it, and the roster matches the window's app too: a
    /// Notes window showing Notes' primary record is where an agent writing a note there is, and
    /// following it brings that window forward rather than opening Notes again. A window drawn by
    /// its app's own surface names no app here; the shell's tree says which app it runs.
    pub fn shown_records(&self) -> Vec<(u64, Option<String>, String)> {
        self.records
            .borrow()
            .iter()
            .map(|(tab, checkout)| (*tab, None, checkout.id.route().to_owned()))
            .collect()
    }
}

/// The workspace's record actions, run by the workspace's host: the page
/// asks the host over the session it already holds with it
/// ([`SessionActions`]), as it asks any other device's actions, and the host
/// runs each through the record's own API as the person. No app code runs in
/// the page for it, so a connector the page never loads (Discord) answers
/// too.
#[derive(Clone, Default)]
pub(crate) struct RecordActions {
    /// The workspace's host, when this page has a session with one.
    host: Option<SessionActions>,
    /// The peer whose workspace this desktop shows, for the records it holds.
    peer: Option<u32>,
    /// The workspace this page is in, and the person asking.
    workspace: String,
    author: String,
    /// What only the page can do with an answer: leave for an address.
    requests: HostRequests,
    /// What waits for the host's answer, by invocation.
    waiting: Waiting,
    /// The answers that came, for the next frame to hand on: the page's own
    /// thread answers what asked, never the session's.
    answered: Arc<std::sync::Mutex<Vec<(InvocationId, ActionAnswer)>>>,
    /// Wakes the mount once an answer is there to hand on.
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
}

/// What waits for one of the host's answers, by the invocation it answers.
type Waiting = Rc<RefCell<HashMap<InvocationId, Box<dyn FnOnce(ActionAnswer)>>>>;

impl RecordActions {
    /// The records' actions `host` runs for this page's person in
    /// `workspace`; `requests` takes what only the page can do, and `wake`
    /// asks for the frame that hands an answer on.
    pub fn new(
        host: Option<SessionActions>,
        peer: Option<u32>,
        workspace: &str,
        author: &str,
        requests: HostRequests,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        Self {
            host,
            peer,
            workspace: workspace.to_owned(),
            author: author.to_owned(),
            requests,
            waiting: Rc::default(),
            answered: Arc::default(),
            wake: Some(wake),
        }
    }

    /// Hand the host's answers to what asked for them; whether any came.
    pub fn follow(&self) -> bool {
        let answers: Vec<(InvocationId, ActionAnswer)> =
            std::mem::take(&mut *self.answered.lock_recover());
        let any = !answers.is_empty();
        for (invocation, answer) in answers {
            if let Some(done) = self.waiting.borrow_mut().remove(&invocation) {
                done(answer);
            }
        }
        any
    }
}

impl tilo_app::linked_accounts::WorkspaceResourceActions for RecordActions {
    fn apps(&self) -> Vec<String> {
        let workspace_apps = self
            .peer
            .map(crate::replicated::records)
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, record)| record.is_empty())
            .map(|(app, _)| app)
            .filter(|app| app != crate::replicated::DESKTOP_APP);
        // An app this page can open runs here, not on the workspace's host: no
        // connector, so its record is not asked for a reading.
        let here = known_applications();
        tilo_app::linked_accounts::connector_apps(workspace_apps, |app| {
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
        let Some(host) = &self.host else {
            done(Err("this page reaches no workspace host".to_owned()));
            return;
        };
        let request = tilo_app::linked_accounts::record_invocation(
            tilo_actions::InvocationId::new(),
            app,
            action,
            arguments,
        );
        let caller = tilo_actions::remote::Caller::local_person(self.author.clone())
            .in_workspace(self.workspace.clone());
        let invocation = request.invocation;
        self.waiting.borrow_mut().insert(invocation, done);
        let answered = Arc::clone(&self.answered);
        let wake = self.wake.clone();
        host.invoke(&caller, request).then(move |answered_| {
            answered.lock_recover().push((
                invocation,
                tilo_app::linked_accounts::resource_answer(answered_),
            ));
            if let Some(wake) = &wake {
                wake();
            }
        });
    }

    fn open(&self, url: &str) {
        // Only a web address: the page leaves for it.
        if url.starts_with("https://") || url.starts_with("http://") {
            self.requests
                .borrow_mut()
                .push(HostRequest::Open(url.to_owned()));
        }
    }
}

/// The person's linked accounts in this desktop's workspace, asked of the
/// workspace's host through `shared`'s workspace actions; an answer wakes the mount.
/// `shared` is the mount's before it holds them, so they hold no cycle.
fn linked_accounts(
    shared: &Shared,
    notify: &Arc<dyn Fn() + Send + Sync>,
) -> tilo_app::linked_accounts::LinkedAccounts {
    let (woken, notify) = (shared.clone(), Arc::clone(notify));
    tilo_app::linked_accounts::LinkedAccounts::new(Rc::new(
        tilo_app::linked_accounts::WorkspaceAccounts::new(
            Rc::new(shared.resource_actions.clone()),
            Rc::new(move || {
                woken.wake();
                notify();
            }),
        ),
    ))
}

struct Tiles {
    inputs: Inputs,
    shared: Shared,
    stores: HashMap<&'static str, ResourceStore>,
    notify: Arc<dyn Fn() + Send + Sync>,
}

impl Tiles {
    fn store(
        &mut self,
        definition: &'static ApplicationDefinition,
    ) -> Result<ResourceStore, String> {
        if let Some(store) = self.stores.get(definition.app_id()) {
            return Ok(store.clone());
        }
        let store =
            self.shared
                .workspace_store(&self.inputs.owners, &self.inputs.workspace, definition)?;
        self.stores.insert(definition.app_id(), store.clone());
        Ok(store)
    }

    /// The app `tab` shows once it can be built here, or the line the
    /// window shows instead.
    fn ready_app<'t>(&self, tab: &'t Tab) -> Result<&'t str, Unready> {
        if let TileContent::HostedWindow { label, .. } = &tab.tile {
            // A window that runs on another device: drawn there, not in a browser.
            let label = if label.is_empty() { &tab.title } else { label };
            return Err(Unready::Notice(format!(
                "{label} is not shown in a browser"
            )));
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
        let definition = application(app_id).ok_or_else(|| not_in_the_browser(app_id))?;
        let unbound = self.shared.unbound.borrow().get(&tab.id).cloned();
        if let Some(id) = unbound.as_deref().and_then(DocumentId::from_route) {
            self.shared.unbound.borrow_mut().remove(&tab.id);
            let checkout = DocumentCheckout::new(id, definition.resource_type.clone());
            self.shared.records.borrow_mut().insert(tab.id, checkout);
        }
        if definition.app_id() == tilo_desktop::feedback::PACKAGE {
            return Ok(self.private_feedback(tab.id)?);
        }
        let store = self.store(definition)?;
        let (target, named_new) = self.launch_target(definition, &store, tab.id)?;
        // A member's own record is theirs to start whatever their role; the
        // workspace's record waits for someone who writes there.
        let own = definition.member_resource && named_new.is_some();
        if !own && (named_new.is_some() || matches!(target, LaunchTarget::PrimaryNew)) {
            self.may_start_record(tab)?;
        }
        // The view this window saved, never one a closed window left under the same id.
        let restore = crate::app_page::saved_view(
            &self.inputs.views,
            tab.id,
            definition,
            &target,
            named_new.as_ref(),
        )
        .map_err(|error| error.to_string())?;
        let component = self
            .instantiate(
                definition,
                store,
                (target, named_new),
                (tab.id, false),
                restore,
            )
            .map_err(|unbuilt| match unbuilt {
                // The record the window shows is still on its way from the workspace: it is
                // loading, not broken, and the window is built again once it is here.
                Unbuilt::Arriving => {
                    self.shared.waiting.borrow_mut().insert(tab.id);
                    self.shared.awaiting_resource()
                }
                Unbuilt::Failed(detail) => Unready::Failed(detail),
            })?;
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

    /// `definition`'s app on `target` (or the new record `named_new`
    /// names), as instance `instance`, mounted to wake this desktop when
    /// its records change.
    fn instantiate(
        &self,
        definition: &'static ApplicationDefinition,
        store: ResourceStore,
        (target, named_new): (LaunchTarget, Option<DocumentId>),
        (instance, embedded): (TabId, bool),
        restore: RestoreInput,
    ) -> Result<AppComponent, Unbuilt> {
        // What the window opens beside its own resource: until its code is in, the window shows
        // its app arriving, and is built again once it is.
        let resources = crate::workspace_resources::WORKSPACE_RESOURCES
            .get()
            .ok_or(Unbuilt::Arriving)?;
        let inputs = ProcessContextInputs {
            records: store,
            views: Rc::clone(&self.inputs.views),
            device_id: self.inputs.device.clone(),
            instance_id: instance,
            permissions: self.inputs.permissions.clone(),
            author: MemberId::person(self.inputs.author.clone()),
            action_context: tilo_actions::ActionContext::default(),
        };
        let mut context = match named_new {
            Some(id) => ProcessContext::prepare_new_resource(definition, inputs, id),
            None => ProcessContext::prepare(definition, inputs, &target),
        }
        .map_err(Unbuilt::of)?;
        context.provide_models(self.inputs.models.clone());
        context.provide_content(self.inputs.content.clone());
        if embedded {
            // A view another app's window embeds: its app shows the one thing asked for.
            context.provide_embedded();
        }
        // What another window handed this one to open, from the desktop's
        // content, where that window keeps it.
        let handed = self.shared.opened_content.borrow_mut().remove(&instance);
        if let Some(hash) = handed {
            let name = self.shared.content_names.borrow().get(&hash).cloned();
            let (file, _) = crate::content_open::opened_file(
                &hash,
                &self.inputs.content,
                name.as_deref(),
                self.inputs
                    .peer
                    .map(|peer| (peer, self.inputs.workspace.as_str())),
            )?;
            context.provide_open_file(file);
        }
        if let Some(page) = self.shared.page_stores(&self.inputs.author) {
            (resources.beside)(&mut context, &page, &self.inputs.store)?;
        }
        crate::host_services::provide_provider_admin(
            &mut context,
            self.inputs.provider_admin.clone(),
        );
        crate::page_content::serve_window(
            &context.wake_handle(),
            &self.shared.images,
            &self.shared.image_owners,
        );
        let mut component = definition
            .instantiate(context, target, restore)
            .map_err(Unbuilt::of)?;
        crate::capture_declarations::note(definition.app_id(), component.capture_protected());
        let woken = Arc::clone(&self.shared.woken);
        let wakers = Arc::clone(&self.shared.wakers);
        let notify = Arc::clone(&self.notify);
        let app_id = definition.app_id();
        component
            .mount_with_owner_queue(&self.shared.owner_wakes, move |event| {
                // A save that moved on asks for the owner's service alone;
                // the service asks for the frame if it shows something.
                if event.asks_for_frame() {
                    woken.store(true, Ordering::Release);
                    let mut wakers = wakers.lock_recover();
                    if !wakers.contains(&app_id) {
                        wakers.push(app_id);
                    }
                }
                notify();
            })
            .map_err(|error| error.to_string())?;
        Ok(component)
    }

    /// `view`'s app on this page's copy of its record, for a window that
    /// embeds it. A record not here yet is said so, never created.
    fn embedded(&mut self, view: &ResourceView) -> Result<TileHost, String> {
        let title = known_application(&view.app).ok_or_else(|| not_in_the_browser(&view.app))?;
        may_open(&self.inputs.permissions.borrow(), &view.app, true)
            .map_err(|_| format!("You can't open {title} here"))?;
        // An app whose code the page has not fetched yet is not among the
        // loaded ones: fetch it, and the view is tried again once it is in.
        if ui::application::registry::is_deferred(&view.app) {
            self.load(&view.app);
            let shape = ui::application::registry::declared_arrival_shape(&view.app);
            return Ok(TileHost::new(ui::widget::TileContent::Arriving).with_arrival_shape(shape));
        }
        let definition = application(&view.app)
            .filter(|definition| definition.app_id() != tilo_desktop::feedback::PACKAGE)
            .ok_or_else(|| not_in_the_browser(&view.app))?;
        let id = DocumentId::from_route(&view.record).ok_or("It is no longer there")?;
        let store = self.store(definition)?;
        let checkout = DocumentCheckout::new(id, definition.resource_type.clone());
        match store.try_open_resource(&checkout) {
            Ok(_) => {}
            Err(
                ui::resource::ResourceOpenError::MissingResource
                | ui::resource::ResourceOpenError::Arriving,
            ) => {
                return Err("It shows once it reaches you".into());
            }
            Err(error) => return Err(error.to_string()),
        }
        // A view of something in the record (a note of the notebook)
        // shows that, as a window opened on it would.
        let reveal = Shared::narrower(&view.app, &view.record, &view.resource).then(|| {
            tilo_actions::ActionAddress::new(
                &view.resource,
                tilo_actions::remote::APPLICATION_OPEN_ACTION,
            )
        });
        let target = LaunchTarget::Record { checkout, reveal };
        let component = self
            .instantiate(
                definition,
                store,
                (target, None),
                (embed_instance(view), true),
                RestoreInput::None,
            )
            .map_err(|unbuilt| match unbuilt {
                Unbuilt::Arriving => "It shows once it reaches you".to_owned(),
                Unbuilt::Failed(detail) => detail,
            })?;
        Ok(TileHost::from_component(component))
    }
}

/// The instance id an embedded view runs as: one per view, clear of the
/// desktop's tab ids, which are minted below the top bit.
fn embed_instance(view: &ResourceView) -> TabId {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    view.hash(&mut hasher);
    hasher.finish() | (1 << 63)
}

/// Why a window cannot show its app yet.
enum Unready {
    /// The app's code is still being fetched.
    Arriving,
    /// What the window says instead.
    Notice(String),
    /// The app could not be built on the record the window shows; what the launch answered.
    Failed(String),
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
        // window reserves its place until then (`ready_app`).
        let code = crate::feedback_composer::code()
            .ok_or_else(|| "Feedback could not be loaded".to_owned())?;
        let services = self
            .shared
            .feedback
            .borrow()
            .as_ref()
            .map(|feedback| feedback.services());
        let mut component =
            (code.prepare)(services, &self.inputs.device, &self.inputs.author, instance)
                .map_err(|error| error.to_string())?;
        let woken = Arc::clone(&self.shared.woken);
        let notify = Arc::clone(&self.notify);
        component
            .mount_with_owner_queue(&self.shared.owner_wakes, move |event| {
                if event.asks_for_frame() {
                    woken.store(true, Ordering::Release);
                }
                notify();
            })
            .map_err(|error| error.to_string())?;
        // Its ephemeral draft owner never enters Shared.stores/records:
        // neither replication nor desktop restore can publish raw text.
        Ok(component)
    }

    /// Whether this page may start the workspace's own record of `tab`'s app,
    /// which this page does not hold yet: in a workspace, only someone who
    /// writes there starts it. Anyone else's window waits for the workspace's
    /// record (it opens once it arrives, or once the workspace says they write)
    /// instead of showing a fresh one of its own as if it were the workspace's.
    fn may_start_record(&self, tab: &Tab) -> Result<(), Unready> {
        if self.shared.resource_actions.peer.is_none() {
            return Ok(());
        }
        let notice = match self.shared.role.role() {
            Some(role) if role.includes(tilo_action_policy::Role::Editor) => return Ok(()),
            None => format!("Opening {}…", tab.title),
            Some(_) => format!(
                "{} has nothing in this workspace yet. It shows here once someone who writes in the workspace starts it.",
                tab.title
            ),
        };
        self.shared.waiting.borrow_mut().insert(tab.id);
        Err(Unready::Notice(notice))
    }

    /// The record `tab` opens: its saved one, the person's own record of an
    /// app each member keeps their own of, the workspace's own record, or a
    /// new one.
    fn launch_target(
        &self,
        definition: &'static ApplicationDefinition,
        store: &ResourceStore,
        tab: TabId,
    ) -> Result<(LaunchTarget, Option<DocumentId>), Unready> {
        let saved = self.shared.records.borrow().get(&tab).cloned();
        match saved {
            Some(checkout) => Ok((
                LaunchTarget::Record {
                    checkout,
                    reveal: None,
                },
                None,
            )),
            None => self
                .own_target(definition, store, tab)
                .unwrap_or_else(|| self.workspace_target(definition, store, tab)),
        }
    }

    /// The person's own record of an app that keeps one per member
    /// ([`crate::app_page::own_opening`]), made on first use.
    fn own_target(
        &self,
        definition: &'static ApplicationDefinition,
        store: &ResourceStore,
        tab: TabId,
    ) -> Option<Result<(LaunchTarget, Option<DocumentId>), Unready>> {
        let author = MemberId::person(self.inputs.author.clone());
        let opening = crate::app_page::own_opening(definition, store, &author)?;
        Some(match opening {
            Ok(crate::app_page::Opening::Target(target)) => Ok((target, None)),
            Ok(crate::app_page::Opening::New(id)) => Ok((LaunchTarget::New, Some(id))),
            Ok(crate::app_page::Opening::Arriving) => {
                Err(self.unopened(tab, &ui::resource::ResourceOpenError::Arriving))
            }
            Err(crate::app_page::Unopened::Records(error)) => Err(self.unopened(tab, &error)),
            Err(crate::app_page::Unopened::InvalidRecord) => {
                Err(Unready::Notice("invalid record id".into()))
            }
        })
    }

    /// The record a window that names none opens in the workspace: the
    /// workspace's own record of an app that keeps one, else its default.
    fn workspace_target(
        &self,
        definition: &'static ApplicationDefinition,
        store: &ResourceStore,
        tab: TabId,
    ) -> Result<(LaunchTarget, Option<DocumentId>), Unready> {
        if !definition.keeps_workspace_resource() {
            return self.unbound_target(definition, store, tab);
        }
        let target = definition
            .unsaved_target(store)
            .map_err(|error| self.unopened(tab, &error))?;
        Ok((target, None))
    }

    /// Why `tab`'s record did not open: one the workspace still sends (or
    /// still has to say it has none of) keeps the window waiting for it,
    /// built again once it is here, instead of starting one of its own.
    fn unopened(&self, tab: TabId, error: &ui::resource::ResourceOpenError) -> Unready {
        if matches!(error, ui::resource::ResourceOpenError::Arriving) {
            self.shared.waiting.borrow_mut().insert(tab);
            return self.shared.awaiting_resource();
        }
        Unready::Notice(error.to_string())
    }

    /// What a tab no record is bound to opens: the workspace's default record
    /// while no other window of the app shows it (the record a native
    /// launch into the workspace opens too), otherwise a new one. The second
    /// value names the default record when it has to be created.
    fn unbound_target(
        &self,
        definition: &'static ApplicationDefinition,
        store: &ResourceStore,
        tab: TabId,
    ) -> Result<(LaunchTarget, Option<DocumentId>), Unready> {
        let default = DocumentCheckout::new(
            DocumentId::workspace_default(),
            definition.resource_type.clone(),
        );
        let taken = self
            .shared
            .records
            .borrow()
            .iter()
            .any(|(other, checkout)| *other != tab && checkout.id == default.id);
        if taken {
            return Ok((LaunchTarget::New, None));
        }
        match store.try_open_resource(&default) {
            Ok(_) => Ok((
                LaunchTarget::Record {
                    checkout: default,
                    reveal: None,
                },
                None,
            )),
            Err(ui::resource::ResourceOpenError::MissingResource) => {
                Ok((LaunchTarget::New, Some(default.id)))
            }
            Err(error) => Err(self.unopened(tab, &error)),
        }
    }
}

impl TileSource for Tiles {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        // Another device's window whose pictures or page travel here: this page draws it, as the
        // native shell does.
        if let TileContent::HostedWindow { app_id, label, .. } = &tab.tile {
            let title = if label.is_empty() { &tab.title } else { label };
            if let Some(host) = crate::window_pictures::tile(tab.id, title, app_id)
                .or_else(|| crate::pages::tile(tab.id, &tab.title))
            {
                return host;
            }
        }
        let shared = self.shared.clone();
        let id = tab.id;
        if let Some(notice) =
            crate::crash::quarantined(tab.id, &tab.title, move || shared.reopen(id))
        {
            return TileHost::new(notice);
        }
        match self.component(tab) {
            Ok(component) => {
                self.shared.waiting.borrow_mut().remove(&tab.id);
                self.shared.unopened.borrow_mut().remove(&tab.id);
                self.shared.landed.borrow_mut().insert(tab.id);
                // Built, but its first frame is not drawn yet: the window
                // stands with what the app declared for that time (its
                // manifest's `arrival-skeleton`) until it presents.
                let host = TileHost::from_component(component);
                match &tab.tile {
                    TileContent::LocalApp { app_id, .. } => host.with_arrival_shape(
                        ui::application::registry::declared_arrival_shape(app_id),
                    ),
                    _ => host,
                }
            }
            // The app's code is on its way: its window reserves the place,
            // showing the shape the app declared for it (its manifest's
            // `arrival-skeleton`), and the app goes on with that arrival once
            // built.
            Err(Unready::Arriving) => {
                self.shared.unopened.borrow_mut().remove(&tab.id);
                self.shared.landed.borrow_mut().remove(&tab.id);
                let TileContent::LocalApp { app_id, .. } = &tab.tile else {
                    return TileHost::new(ui::widget::TileContent::Arriving)
                        .arriving_from(ui::arriving::Source::Remote);
                };
                let shape = ui::application::registry::declared_arrival_shape(app_id);
                TileHost::new(ui::widget::TileContent::Arriving)
                    .arriving_from(ui::arriving::Source::Remote)
                    .with_arrival_shape(shape)
            }
            Err(Unready::Notice(detail)) => {
                self.shared
                    .unopened
                    .borrow_mut()
                    .insert(tab.id, detail.clone());
                self.shared.landed.borrow_mut().remove(&tab.id);
                if self.shared.records.borrow().contains_key(&tab.id) {
                    self.shared.waiting.borrow_mut().insert(tab.id);
                }
                TileHost::new(ui::widget::TileContent::Notice {
                    title: tab.title.clone(),
                    detail,
                })
            }
            // The app was not built: the window says that it could not open and what to do, in
            // the person's words, and tries again on their word. What the launch answered is for
            // whoever looks into it: behind the card's Details, and in the console.
            Err(Unready::Failed(detail)) => {
                self.shared
                    .unopened
                    .borrow_mut()
                    .insert(tab.id, detail.clone());
                self.shared.landed.borrow_mut().remove(&tab.id);
                if self.shared.records.borrow().contains_key(&tab.id) {
                    self.shared.waiting.borrow_mut().insert(tab.id);
                }
                web_sys::console::warn_1(&format!("{} could not open: {detail}", tab.title).into());
                let outcome = tilo_desktop_surface::outcome::Outcome::not_built(&tab.title, detail);
                let shared = self.shared.clone();
                let id = tab.id;
                TileHost::new(ui::widget::TileContent::App(Box::new(
                    tilo_desktop_surface::outcome::OutcomeCard::new(&outcome, move || {
                        shared.reopen(id);
                    }),
                )))
            }
        }
    }

    fn launch(&mut self, _: &Workspace, id: TabId, app: &str, _: Option<TabId>) -> Option<Tab> {
        let (app, record, content) =
            if let Some((app, hash)) = tilo_desktop_surface::session::launched_content(app) {
                (app, None, Some(hash))
            } else if let Some((app, record)) = tilo_desktop_search::launched_record(app) {
                (app, Some(record), None)
            } else {
                (app, None, None)
            };
        let title = known_application(app)?;
        if let Some(hash) = content {
            self.shared
                .opened_content
                .borrow_mut()
                .insert(id, hash.to_owned());
        }
        if let Some(record) = record {
            // The window opens on the record a shared link or window named.
            self.shared
                .unbound
                .borrow_mut()
                .insert(id, record.to_owned());
        }
        self.shared
            .authors
            .borrow_mut()
            .insert(id, self.inputs.author.clone());
        Some(app_tab(id, app, title))
    }

    fn showing(&self, app: &str) -> Option<TabId> {
        let (app, record) = tilo_desktop_search::launched_record(app)?;
        let kind = application(app)?.resource_type.clone();
        let bound = self
            .shared
            .records
            .borrow()
            .iter()
            .find(|(_, checkout)| checkout.id.route() == record && checkout.kind == kind)
            .map(|(tab, _)| *tab);
        // A window the shared shell shows on the record, whose record this page
        // has not bound yet (its bytes are still on their way): the same
        // window, not a second one on the same document. A record launch names
        // a record by its own id, never the app's primary record.
        bound.or_else(|| {
            self.shared
                .unbound
                .borrow()
                .iter()
                .find(|(_, held)| held.as_str() == record)
                .map(|(tab, _)| *tab)
        })
    }

    fn window_resource(&self, tab: TabId) -> Option<(String, String)> {
        self.shared.records.borrow().get(&tab).map(|checkout| {
            (
                checkout.id.route().to_owned(),
                checkout.kind.as_str().to_owned(),
            )
        })
    }

    fn workspace_id(&self) -> Option<String> {
        Some(self.inputs.workspace.clone())
    }

    fn draws_embeds(&self) -> bool {
        true
    }

    fn embed(&mut self, view: &ResourceView) -> Result<TileHost, String> {
        self.embedded(view)
    }

    fn search_applications(&self) -> Vec<tilo_desktop_search::providers::AppEntry> {
        web_applications().collect()
    }
}

/// The applications this page offers to open itself: those of the bundle that run on the web.
fn web_applications() -> impl Iterator<Item = tilo_desktop_search::providers::AppEntry> {
    known_applications()
        .into_iter()
        .filter(|(id, _)| tilo_desktop_search::availability::runs_on(id, &["web"]))
        .map(|(id, title)| tilo_desktop_search::providers::AppEntry {
            id: id.into(),
            title: title.into(),
            category: None,
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
        view: String::new(),
        pinned: false,
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

/// Where a workspace's desktop lives in the profile. Changing it orphans stored
/// desktops.
pub(crate) const DESKTOP_PACKAGE: &str = "so.tilo.browser-desktop";

/// Saves a mounted desktop to the profile whenever its tree or its tabs'
/// records change: the layout's own document
/// ([`tilo_desktop_model::desktop_doc`]), whose changes are what happened to it
/// and who did it, and which a workspace replicates as it stands. Writes are
/// accepted by the durable store and committed to `IndexedDB` by the page's
/// durable host.
pub(crate) struct Persistence {
    store: Rc<dyn DurableStore>,
    saved_revision: Cell<u64>,
    records_changed: Rc<Cell<bool>>,
    records: Rc<RefCell<HashMap<TabId, DocumentCheckout>>>,
    unbound: Rc<RefCell<HashMap<TabId, String>>>,
    authors: Authors,
    /// The layout's own document: the profile's copy of it, which is the layout's
    /// history when this page is in no workspace. The rules of the document —
    /// what one change of the layout is, who made it, how a step is taken back —
    /// are the session core's, the same the native session runs. Shared with
    /// the page's layout, which changes that layout by action.
    layout: Rc<LayoutDocument>,
    /// Where the document is kept, and why the last write of it was refused.
    profile: Rc<ProfileStore>,
    /// The peer whose workspace this page joined, whose copy of the layout document
    /// is the one that counts once the workspace answered
    /// ([`Self::mirror_workspace`]).
    peer: Option<u32>,
    pub notifications: tilo_desktop_surface::DesktopNotifications,
    notifications_key: StorageKey,
    saved_notifications_revision: Cell<u64>,
}

impl Persistence {
    /// The layout document, as the page's layout keeps it too.
    pub(crate) fn layout(&self) -> Rc<LayoutDocument> {
        Rc::clone(&self.layout)
    }

    /// Write the desktop if it changed since the last save.
    ///
    /// # Errors
    /// The store refused the write; the next change retries.
    pub fn save(&self, handle: &DesktopHandle) -> Result<(), String> {
        self.save_notifications()?;
        let mirrored = self.mirror_workspace()?;
        let revision = handle.revision();
        if !mirrored && revision == self.saved_revision.get() && !self.records_changed.get() {
            return Ok(());
        }
        // The document keeps how this device shows the shell — the widths, the
        // workspace and the window in front — so a reload returns the shell as
        // it was left, as native's own document does.
        let tree = handle.read(Clone::clone);
        let time = ui::time::unix_now_i64();
        if self
            .peer
            .and_then(crate::replicated::desktop_history_heads)
            .is_some()
        {
            // The workspace holds the shell itself; the copy keeps the view alone.
            self.layout.record_view(&tree, time);
        } else {
            // No workspace holds this shell: what the person changed is this page's
            // change of its own document.
            let records = tilo_desktop_surface::host::records::live(
                &self.records.borrow(),
                &self.unbound.borrow(),
                &tree,
            );
            let authors = self.authors.borrow();
            self.layout
                .record_now(&tree, time, &records, Some(&authors));
        }
        if mirrored {
            // The workspace's copy replaced this page's: what it holds is what
            // the profile keeps from now on.
            self.layout.keep_now();
        }
        if let Some(error) = self.profile.refused() {
            return Err(error);
        }
        self.saved_revision.set(revision);
        self.records_changed.set(false);
        Ok(())
    }

    /// Take the workspace's layout document into the profile's copy, once the page's
    /// layout is the workspace's: a reload opens it as the workspace has it, and what
    /// the workspace changed reaches the file as the layout's own history.
    /// Says whether it took a newer one.
    fn mirror_workspace(&self) -> Result<bool, String> {
        let Some(peer) = self.peer else {
            return Ok(false);
        };
        let Some(heads) = crate::replicated::desktop_history_heads(peer) else {
            return Ok(false);
        };
        if heads
            .iter()
            .all(|head| self.layout.document().get_change_by_hash(head).is_some())
        {
            return Ok(false);
        }
        let Some(bytes) = crate::replicated::layout_bytes(peer) else {
            return Ok(false);
        };
        self.layout
            .replace(AutoCommit::load(&bytes).map_err(|error| error.to_string())?);
        Ok(true)
    }

    /// Keep what the person's page offered to answer: a notice kept past a
    /// reload could no longer be answered.
    fn save_notifications(&self) -> Result<(), String> {
        let notifications_revision = self.notifications.revision();
        if notifications_revision == self.saved_notifications_revision.get() {
            return Ok(());
        }
        let bytes = self
            .notifications
            .kept(ui::time::wall())
            .map_err(|error| error.to_string())?;
        self.store
            .write(&self.notifications_key, WriteOp::Replace(bytes), None)
            .map_err(|error| error.to_string())?;
        self.saved_notifications_revision
            .set(notifications_revision);
        Ok(())
    }
}

/// Where the profile's copy of the shell's document is kept, and why the store
/// refused the last write of it.
struct ProfileStore {
    store: Rc<dyn DurableStore>,
    key: StorageKey,
    refused: RefCell<Option<String>>,
}

impl ProfileStore {
    /// Why the last write was refused, taken so the next one starts clean.
    fn refused(&self) -> Option<String> {
        self.refused.borrow_mut().take()
    }
}

impl Store for ProfileStore {
    fn keep(&self, bytes: Vec<u8>) {
        if let Err(error) = self.store.write(&self.key, WriteOp::Replace(bytes), None) {
            *self.refused.borrow_mut() = Some(error.to_string());
        }
    }
}

/// The desktop as peers share it: the tree and each tab's record.
pub(crate) fn shared_value(
    handle: &DesktopHandle,
    shared: &Shared,
) -> tilo_desktop_model::ownership::SharedDesktop {
    let tree = handle.shared_tree();
    let records = tilo_desktop_surface::host::records::live(
        &shared.records.borrow(),
        &shared.unbound.borrow(),
        &tree,
    );
    let authors = tilo_session_core::layout::authors_on(&tree, &shared.authors.borrow());
    tilo_desktop_model::ownership::SharedDesktop {
        tree,
        records,
        authors,
        opened_by: shared.opened_by.borrow().clone(),
    }
}

/// Take a desktop a peer shared: bind its tabs' records, then hand the tree
/// to the surface's reconcile.
pub(crate) fn apply_shared(
    handle: &DesktopHandle,
    shared: &Shared,
    value: &serde_json::Value,
) -> Result<(), String> {
    let desktop: tilo_desktop_model::ownership::SharedDesktop =
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
    if desktop.tree.workspaces.is_empty() {
        return Err("a desktop needs a workspace".into());
    }
    {
        let (bound, unbound) = adopted_records(&desktop);
        shared.unbound.borrow_mut().extend(unbound);
        let mut authors = shared.authors.borrow_mut();
        for (id, author) in &desktop.authors {
            authors.entry(*id).or_insert_with(|| author.clone());
        }
        drop(authors);
        shared.opened_by.borrow_mut().clone_from(&desktop.opened_by);
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
    handle.replace_shared_tree(desktop.tree, &[]);
    shared.wake();
    Ok(())
}

/// Put the workspace's desktop `value` back over a local change its person may
/// not make, attribution included: what the workspace says made each strip and
/// window is what this desktop says too.
///
/// # Errors
/// As [`apply_shared`].
pub(crate) fn return_to(
    handle: &DesktopHandle,
    shared: &Shared,
    value: &serde_json::Value,
) -> Result<(), String> {
    let desktop: tilo_desktop_model::ownership::SharedDesktop =
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

/// The record each window of a stored desktop shows, as this page takes it
/// over: bound where the window's application runs here, and held under the
/// shell's own spelling otherwise ([`tilo_desktop_surface::host::records::adopted`]).
fn adopted_records(
    desktop: &tilo_desktop_model::ownership::SharedDesktop,
) -> (HashMap<TabId, DocumentCheckout>, HashMap<TabId, String>) {
    tilo_desktop_surface::host::records::adopted(desktop, |app| {
        application(app).map(|definition| definition.resource_type.clone())
    })
}

/// The layout document this profile keeps, or an empty one when it holds none.
fn open_layout(store: &dyn DurableStore, key: &StorageKey) -> AutoCommit {
    store
        .read(key)
        .ok()
        .flatten()
        .filter(|stored| !stored.bytes.is_empty())
        .and_then(|stored| AutoCommit::load(&stored.bytes).ok())
        .unwrap_or_default()
}

/// Mount the workspace's desktop from the profile, or seed one: one workspace
/// named after the workspace, one strip, a window per requested application
/// (the first registered one by default).
///
/// # Errors
/// Refuses an application id the page does not register.
/// Install the page's feedback startup in a workspace's desktop, or take it up
/// once the page's destination is admitted: the desktop never waits for it.
fn take_up_feedback(
    shared: &Shared,
    inputs: &mut Inputs,
    store: &Rc<dyn DurableStore>,
    notify: &Arc<dyn Fn() + Send + Sync>,
) {
    let Some(peer) = inputs.peer else {
        return;
    };
    let late = LateFeedback {
        peer,
        wake: Arc::clone(notify),
        tries: 0,
        author: inputs.author.clone(),
        store: Rc::clone(store),
        models: inputs.models.clone(),
        model_name: inputs.model_name.clone(),
    };
    match inputs.feedback.take() {
        Some(startup) => {
            shared.settle_feedback(late, startup);
        }
        None => shared.await_feedback(late),
    }
}

/// What a page's desktop knows before it mounts: the session it holds with
/// the workspace's host, which runs its records' actions, and the person's
/// linked accounts, which that host answers.
fn shared_with_its_host(inputs: &Inputs, notify: &Arc<dyn Fn() + Send + Sync>) -> Shared {
    let requests = crate::shell::HostRequests::default();
    let host = inputs.peer.and_then(|peer| {
        let workspace = crate::peer::workspace_id(&inputs.workspace).ok()?;
        crate::peer_sync::actions_of(peer, workspace)
    });
    let resource_actions = RecordActions::new(
        host,
        inputs.peer,
        &inputs.workspace,
        &inputs.author,
        Rc::clone(&requests),
        Arc::clone(notify),
    );
    let mut shared = Shared {
        resource_actions,
        requests,
        owners: Some(inputs.owners.clone()),
        peer: inputs.peer,
        workspace: inputs.workspace.clone(),
        ..Shared::default()
    };
    if inputs.peer.is_some() {
        // In a workspace, the person holds what the workspace answers, once it has.
        shared.role = SharedRole::unknown();
    }
    shared.linked = linked_accounts(&shared, notify);
    shared
}

/// A window's state as the page's status says it: `{"state": …}`, with the `reason` of a window
/// that could not open.
fn window_state_json(state: &crate::window_states::WindowState) -> serde_json::Value {
    use crate::window_states::WindowState;
    match state {
        WindowState::Arriving => serde_json::json!({"state": "arriving"}),
        WindowState::Landed => serde_json::json!({"state": "landed"}),
        WindowState::Drawn => serde_json::json!({"state": "drawn"}),
        WindowState::CouldNotOpen { reason } => {
            serde_json::json!({"state": "could-not-open", "reason": reason})
        }
    }
}

pub(crate) fn mount(
    mut inputs: Inputs,
    store: Rc<dyn DurableStore>,
    apps: Option<&[String]>,
    workspace_name: Option<&str>,
    notify: Arc<dyn Fn() + Send + Sync>,
) -> Result<(DesktopSurface, DesktopHandle, Shared, Persistence), String> {
    let key = StorageKey::document(DESKTOP_PACKAGE, inputs.workspace.clone(), "desktop");
    let salt = peer_salt(&inputs.salt);
    let shared = shared_with_its_host(&inputs, &notify);
    take_up_feedback(&shared, &mut inputs, &store, &notify);
    let document = open_layout(store.as_ref(), &key);
    // In a workspace the profile keeps the workspace's shell, which holds nobody's own windows
    // (`tilo_desktop_model::personal`); a shell no workspace shares is this person's alone, all of
    // it.
    let stored = if inputs.peer.is_some() {
        tilo_desktop_model::shared_resource::read(&document, None)
    } else {
        tilo_desktop_model::shared_resource::read_as_kept(&document, None)
    };
    let tree = if let Some(stored) = stored {
        let (bound, unbound) = adopted_records(&stored);
        *shared.records.borrow_mut() = bound;
        *shared.unbound.borrow_mut() = unbound;
        *shared.authors.borrow_mut() = stored.authors;
        // How this device showed the shell — the strip and window in front,
        // the widths, the displays — as it was left, as native opens it.
        let mut tree = stored.tree;
        tilo_desktop_model::desktop_doc::rebuild_view(&document, &mut tree);
        tree
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
        // A workspace nobody named is kept with no name: the shell names it in the language of
        // whoever reads it.
        let name = workspace_name
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or_default();
        // In a workspace the shell is that workspace's, by its id: the one its records are kept by.
        match tilo_desktop_model::WorkspaceId::parse(&inputs.workspace) {
            Some(identity) => tilo_desktop_surface::seed_tree_in(&mut minter, identity, name, tabs),
            None => tilo_desktop_surface::seed_tree(&mut minter, name, tabs),
        }
    };
    let notifications_key =
        StorageKey::document(DESKTOP_PACKAGE, inputs.workspace.clone(), "notifications");
    let notifications = tilo_desktop_surface::DesktopNotifications::default();
    if let Some(stored) = store.read(&notifications_key).ok().flatten() {
        // A record this page cannot read leaves the centre empty rather than the page unopened.
        let _ = notifications.restore_kept(&stored.bytes, ui::time::wall());
    }
    let saved_notifications_revision = Cell::new(notifications.revision());
    let profile = Rc::new(ProfileStore {
        store: Rc::clone(&store),
        key,
        refused: RefCell::new(None),
    });
    let persistence = Persistence {
        layout: Rc::new(LayoutDocument::new(
            document,
            &inputs.author,
            Some(Rc::clone(&profile) as Rc<dyn Store>),
        )),
        profile,
        store,
        saved_revision: Cell::new(0),
        records_changed: Rc::clone(&shared.records_changed),
        records: Rc::clone(&shared.records),
        unbound: Rc::clone(&shared.unbound),
        authors: Rc::clone(&shared.authors),
        peer: inputs.peer,
        notifications,
        notifications_key,
        saved_notifications_revision,
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
