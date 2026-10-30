//! The native Tilo session: a thin host, as the web page is one.
//!
//! The compositor ([`tilo_compositor`]) shows buffers and routes input. The
//! session gives it the desktop every host shows
//! ([`tilo_desktop::desktop_host`]): the desk (backdrop, launchpad, the
//! strips and window bars of the desktop surface) under the windows, and
//! the chrome (top bar, strip ribbon and rail, overview, menus) above them;
//! the window policy of the desk's strips ([`tilo_surface_host::desktop`]),
//! so each window is a tab of a strip; and the desktop's surface apps, each
//! an ordinary app hosted on a layer of its own: the notification banners
//! and quick settings. What the chrome asks of its host comes back as
//! [`DesktopAsk`]s, carried out here.
//!
//! The session also hosts the system services it was started with
//! ([`Options::services`]): connectivity, audio and the rest publish their
//! resources beside the desktop's, and [`Session::invoke`] reaches their
//! actions as it reaches the desktop's, by address.

#![cfg(target_os = "linux")]

mod chrome;
pub mod desktop_services;
mod lists;
mod panels;
mod search;

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use tilo_actions::{ActionAddress, DoorError, ResourceSpec};
use tilo_compositor::{Compositor, DisplayConfig, Error, OutputConfig, WindowPolicy};
use tilo_desktop::about::{AboutLinks, AboutVariant, TiloMenu};
use tilo_desktop::chrome::ChromeIntent;
use tilo_desktop::desktop_host::{DeskLayer, DesktopAsk, DesktopChrome};
use tilo_desktop::launchpad::{CollaborationLaunchpad, LaunchpadAction};
use tilo_desktop::topbar::TopBarChrome;
use tilo_desktop_surface::DesktopHandle;
use tilo_desktop_surface::navigation::Command;
use tilo_service::Started;
use tilo_surface_host::{DesktopHost, Placement, WidgetHost, desktop_with};
use ui::application::ComponentRunnerInputs;
use ui::headless::FrameHarness;
use ui::host_operation::ResourceOperationStart;

pub use lists::{AppLists, Lists};
pub use search::{AppActions, SearchOptions};
pub use tilo_quick_settings::host::PanelHost;
pub use tilo_service::{Service, Services};

use chrome::{SessionChrome, SessionRules};
use panels::{Banners, Held, Popover};
use search::SessionSearch;
use tilo_desktop_search::providers::places::Place;

/// The generation the session's own desktop commands carry.
const GENERATION: &str = "session";

/// What a session starts with.
pub struct Options {
    /// Seeds the desktop's ids, so two devices never mint the same.
    pub salt: u16,
    /// Physical pixels per logical pixel the desktop draws at.
    pub scale: f64,
    /// This device, by id.
    pub device: String,
    /// The person at it, by the id the desktop's authors name them by.
    pub person: String,
    /// The theme and fonts surface apps draw with.
    pub presentation: fn() -> ComponentRunnerInputs,
    /// What quick settings reads of the device and carries out on it;
    /// without one, the bar's status cluster opens nothing.
    pub quick_settings: Option<Rc<dyn PanelHost>>,
    /// What the desktop's search reaches beyond the desktop.
    pub search: SearchOptions,
    /// The action host: what open apps publish and run, and where what is
    /// chosen in search runs by address. Without one, search finds the
    /// desktop's own and starts nothing.
    pub apps: Option<Rc<dyn AppActions>>,
    /// The system services the session hosts: what they publish is the
    /// session's, beside the desktop's.
    pub services: Vec<Box<dyn Service>>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            salt: 1,
            scale: 1.0,
            device: "this-device".into(),
            person: "person".into(),
            presentation: ComponentRunnerInputs::from_native_process,
            quick_settings: None,
            search: SearchOptions::default(),
            apps: None,
            services: Vec::new(),
        }
    }
}

/// The desktop's two hosted trees: the desk's under the windows and the
/// chrome's above them.
#[derive(Clone)]
struct Layers {
    desktop: DesktopHost,
    chrome: Rc<RefCell<WidgetHost>>,
}

impl Layers {
    /// `read` of the desk's hosted tree, then of the chrome's.
    fn each<R>(&self, mut read: impl FnMut(&mut FrameHarness) -> R) -> [R; 2] {
        let desk = self.desktop.with_widgets(|host| read(host.host_mut()));
        let chrome = read(self.chrome.borrow_mut().host_mut());
        [desk, chrome]
    }

    fn resources(&self) -> Vec<ResourceSpec> {
        self.each(|host| host.resources()).concat()
    }

    /// Where what the desk's and the chrome's trees publish stands.
    fn revisions(&self) -> [u64; 2] {
        [
            self.desktop.with_widgets(|host| host.revision()),
            self.chrome.borrow().revision(),
        ]
    }

    /// Invoke the published action `action` of `resource` on the layer
    /// that publishes it.
    fn invoke(
        &self,
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ResourceOperationStart, DoorError> {
        let address = ActionAddress::new(resource, action);
        let [desk, chrome] = self.each(|host| {
            tilo_actions::door::resolve(&host.resources(), &address)
                .is_some()
                .then(|| host.invoke_published(resource, action, arguments))
        });
        desk.or(chrome)
            .unwrap_or_else(|| Err(DoorError::Unknown(address.clone())))
    }
}

/// What the session keeps between frames, shared with the loop.
struct Shared {
    layers: Layers,
    handle: DesktopHandle,
    search: Rc<SessionSearch>,
    /// What the desk and the chrome asked of the session.
    asks: Rc<RefCell<Vec<DesktopAsk>>>,
    /// Where the chrome takes intents the session raises.
    intents: Rc<RefCell<Vec<ChromeIntent>>>,
    /// Whether quick settings is open, when the session has it.
    quick_settings: Option<Rc<Cell<bool>>>,
    /// The system services.
    services: RefCell<Services>,
    /// Where everything the session publishes stood when search last read
    /// it: the desk's, the chrome's and the services' revisions.
    observed: Cell<Option<[u64; 3]>>,
    /// The title each window had when search last recorded where it was.
    places_seen: RefCell<std::collections::HashMap<u64, String>>,
}

/// The app a window shows, by its id.
fn tab_app(tile: &tilo_desktop_model::TileContent) -> Option<String> {
    use tilo_desktop_model::TileContent;
    match tile {
        TileContent::Foreign { app_id, .. }
        | TileContent::LocalApp { app_id, .. }
        | TileContent::PeerWindow { app_id, .. }
        | TileContent::HostedWindow { app_id, .. } => Some(app_id.clone()),
        _ => None,
    }
}

impl Shared {
    /// Carry out what the desktop asked since the last frame.
    fn follow_asks(&self) {
        let asks = std::mem::take(&mut *self.asks.borrow_mut());
        for ask in asks {
            match ask {
                DesktopAsk::Intent(ChromeIntent::ToggleQuickSettings) => {
                    if let Some(open) = &self.quick_settings {
                        open.set(!open.get());
                    }
                }
                DesktopAsk::Intent(ChromeIntent::ShowQuickSettings) => {
                    if let Some(open) = &self.quick_settings {
                        open.set(true);
                    }
                }
                DesktopAsk::Launchpad(action) => self.launchpad(action),
                other => tracing::debug!(?other, "desktop ask the session has no part in yet"),
            }
        }
        let choices = self.handle.take_search_choices();
        if choices.is_empty() {
            return;
        }
        let room = self.handle.read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.room().to_string())
                .unwrap_or_default()
        });
        for choice in choices {
            self.search.perform(&room, choice, &mut |own| {
                self.invoke(
                    own.address.resource.as_str(),
                    own.address.action.as_str(),
                    &own.arguments,
                )
                .map(drop)
            });
        }
        // What the session's own actions asked of it.
        self.follow_asks();
    }

    /// Everything the session publishes: the desk's and the chrome's
    /// resources, then the services'.
    fn resources(&self) -> Vec<ResourceSpec> {
        let mut resources = self.layers.resources();
        resources.extend(self.services.borrow().resources());
        resources
    }

    /// Invoke the published action `action` of `resource` where it is
    /// published: on the desk's or the chrome's layer, else at the service
    /// publishing it.
    fn invoke(
        &self,
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ResourceOperationStart, DoorError> {
        match self.layers.invoke(resource, action, arguments) {
            Err(DoorError::Unknown(address)) => {
                match self
                    .services
                    .borrow_mut()
                    .invoke(resource, action, arguments)
                {
                    Some(Ok(Started::Done(output))) => {
                        Ok(ResourceOperationStart::Completed(output))
                    }
                    Some(Ok(Started::Accepted)) => Ok(ResourceOperationStart::Accepted),
                    Some(Err(error)) => Err(error),
                    None => Err(DoorError::Unknown(address)),
                }
            }
            other => other,
        }
    }

    /// Where everything the session publishes stands: it may have changed
    /// only when this moved.
    fn revision(&self) -> [u64; 3] {
        let [desk, chrome] = self.layers.revisions();
        [desk, chrome, self.services.borrow().revision()]
    }

    /// Hand search what the session publishes when it changed: read after
    /// a frame, as the desk cannot read its own tree while it opens search.
    fn observe(&self) {
        let now = Some(self.revision());
        if self.observed.replace(now) == now {
            return;
        }
        let tabs = self.handle.read(|tree| {
            let mut tabs = Vec::new();
            for workspace in &tree.workspaces {
                let room = workspace.room().to_string();
                for tab in workspace
                    .strips
                    .iter()
                    .flat_map(|strip| &strip.window_groups)
                    .flat_map(|group| &group.tabs)
                {
                    tabs.push((tab.id, room.clone(), tab_app(&tab.tile), tab.title.clone()));
                }
            }
            tabs
        });
        self.record_places(&tabs);
        let open_tabs = tabs.iter().map(|(tab, ..)| *tab).collect();
        self.search.observe(&self.resources(), open_tabs);
        self.keep_history();
    }

    /// Where each app's window is now, for search's Continue: a window
    /// whose title changed since it was last seen was at a new place.
    fn record_places(&self, tabs: &[(u64, String, Option<String>, String)]) {
        let mut seen = self.places_seen.borrow_mut();
        seen.retain(|tab, _| tabs.iter().any(|(open, ..)| open == tab));
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        for (tab, room, app, title) in tabs {
            let Some(app) = app else { continue };
            if title.trim().is_empty() || seen.get(tab) == Some(title) {
                continue;
            }
            seen.insert(*tab, title.clone());
            self.search.record_place(Place {
                app: app.clone(),
                room: room.clone(),
                tab: *tab,
                label: title.clone(),
                value: String::new(),
                anchor: None,
                at: now,
                device: None,
            });
        }
    }

    /// Keep what changed of search's history in its settings section,
    /// through the section's field edits; without a settings service it
    /// lasts as long as the session.
    fn keep_history(&self) {
        for edit in self.search.take_unkept() {
            let kept = self
                .services
                .borrow_mut()
                .invoke(search::HISTORY, "field-edit", &edit);
            if let Some(Err(error)) = kept {
                tracing::warn!(%error, "search history not kept");
            }
        }
    }

    /// A card of the empty desk's launchpad: its app, else the desktop's
    /// search, which finds what to open.
    fn launchpad(&self, action: LaunchpadAction) {
        match action {
            LaunchpadAction::Activity => self
                .intents
                .borrow_mut()
                .push(ChromeIntent::ShowActivity(None)),
            other => match other.app_target() {
                Some((app, _)) => {
                    let launch = Command::Launch {
                        generation: GENERATION.into(),
                        app: app.into(),
                    };
                    if let Err(error) = self.handle.dispatch(&launch, GENERATION) {
                        tracing::warn!(%error, app, "launchpad");
                    }
                }
                None => self.handle.open_search(None),
            },
        }
    }
}

/// A running session: the compositor and the desktop it hosts.
pub struct Session {
    compositor: Compositor,
    shared: Rc<Shared>,
    banners: tilo_notification_banners::Link,
}

/// The notification banners over the windows, and the link they are fed
/// through.
fn mount_banners(
    compositor: &mut Compositor,
    options: &Options,
) -> tilo_notification_banners::Link {
    let banners = tilo_notification_banners::Link::new();
    match tilo_notification_banners::prepare(banners.clone(), &options.device, &options.person)
        .and_then(|component| ui::App::from_component(component, (options.presentation)()))
    {
        Ok(app) => {
            compositor.add_host(Box::new(Banners::new(app, banners.clone(), options.scale)));
        }
        Err(error) => tracing::warn!(%error, "notification banners not mounted"),
    }
    banners
}

/// Quick settings, when the session has a panel for them: whether they are
/// open.
fn mount_quick_settings(compositor: &mut Compositor, options: &Options) -> Option<Rc<Cell<bool>>> {
    let panel = options.quick_settings.clone()?;
    let open = Rc::new(Cell::new(false));
    match tilo_quick_settings::prepare(panel, &options.device, &options.person, 1)
        .and_then(|component| ui::App::from_component(component, (options.presentation)()))
    {
        Ok(app) => {
            compositor.add_host(Box::new(Popover::new(app, Rc::clone(&open), options.scale)));
            Some(open)
        }
        Err(error) => {
            tracing::warn!(%error, "quick settings not mounted");
            None
        }
    }
}

impl Session {
    /// A session on the compositor `start` makes with the desktop's window
    /// policy.
    ///
    /// # Errors
    /// The compositor did not start.
    pub fn new(
        options: Options,
        start: impl FnOnce(Box<dyn WindowPolicy>) -> Result<Compositor, Error>,
    ) -> Result<Self, Error> {
        // What the desk and the chrome ask of the session, from their own
        // widget trees.
        let asks = Rc::new(RefCell::new(Vec::new()));
        let (desktop, policy) = desktop_with(options.salt, options.scale, |surface, handle| {
            Box::new(
                DeskLayer::new(
                    Box::new(surface),
                    handle.clone(),
                    CollaborationLaunchpad::new(),
                    Vec::new(),
                )
                .with_asks(Rc::clone(&asks)),
            )
        });
        let handle = desktop.with_policy(|policy| policy.handle().clone());
        let mut compositor = start(Box::new(policy))?;
        compositor.add_host(Box::new(desktop.clone()));

        // The chrome above the windows: its own tree, beside the desk's.
        let rules = Rc::new(SessionRules {
            handle: handle.clone(),
            person: options.person.clone(),
            authors: RefCell::new(BTreeMap::new()),
        });
        let tree = handle.read(Clone::clone);
        // The bar's source hands its intents to the chrome it sits in.
        let intents = Rc::new(RefCell::new(Vec::new()));
        let source = Rc::new(SessionChrome::new(
            handle.clone(),
            Rc::clone(&intents),
            options.person.clone(),
        ));
        let bar = TopBarChrome::from_tree(&tree)
            .with_chrome(source)
            .with_tilo_mark();
        let chrome = DesktopChrome::new(
            handle.clone(),
            GENERATION.into(),
            rules,
            bar,
            TiloMenu::new(AboutVariant::Native, &AboutLinks::default()),
            false,
        )
        .with_intents(Rc::clone(&intents))
        .with_asks(Rc::clone(&asks));
        let keyboard = chrome.keyboard();
        let placement = Placement {
            origin: (0, 0),
            size: (1, 1),
            scale: options.scale,
        };
        let chrome = Rc::new(RefCell::new(
            WidgetHost::new(Box::new(chrome), placement, false)
                .covering_outputs()
                .with_keyboard(keyboard),
        ));
        compositor.add_host(Box::new(Held(Rc::clone(&chrome))));

        let banners = mount_banners(&mut compositor, &options);
        let quick_settings = mount_quick_settings(&mut compositor, &options);

        let search = Rc::new(SessionSearch::new(options.search, options.apps));
        handle.set_search_host(Rc::clone(&search) as Rc<dyn tilo_desktop_surface::SearchHost>);
        let mut services = Services::default();
        for service in options.services {
            services.add(service);
        }
        let shared = Rc::new(Shared {
            layers: Layers { desktop, chrome },
            handle,
            search,
            asks,
            intents,
            quick_settings,
            services: RefCell::new(services),
            observed: Cell::new(None),
            places_seen: RefCell::default(),
        });
        if let Some(kept) = shared.services.borrow().read(search::HISTORY) {
            shared.search.restore(&kept);
        }
        shared.observe();
        Ok(Self {
            compositor,
            shared,
            banners,
        })
    }

    /// A session whose outputs have no display: for tests and screenshots.
    ///
    /// # Errors
    /// The compositor or an output did not start.
    pub fn headless(options: Options, outputs: &[OutputConfig]) -> Result<Self, Error> {
        let mut session = Self::new(options, Compositor::headless)?;
        for output in outputs {
            session.compositor.add_output(output)?;
        }
        Ok(session)
    }

    /// A session on a bare TTY.
    ///
    /// # Errors
    /// The compositor did not start.
    pub fn drm(options: Options, config: DisplayConfig) -> Result<Self, Error> {
        Self::new(options, |policy| Compositor::drm(policy, config))
    }

    /// The compositor, to connect clients, route input and read frames.
    pub fn compositor(&mut self) -> &mut Compositor {
        &mut self.compositor
    }

    /// The desktop under the windows and its policy.
    #[must_use]
    pub fn desktop(&self) -> &DesktopHost {
        &self.shared.layers.desktop
    }

    /// The host side of the desktop surface.
    #[must_use]
    pub fn handle(&self) -> &DesktopHandle {
        &self.shared.handle
    }

    /// The notification banners' link: the notifications service publishes
    /// on it and carries out what the banners ask.
    #[must_use]
    pub const fn banners(&self) -> &tilo_notification_banners::Link {
        &self.banners
    }

    /// Everything the session publishes now: the desk's resource (its
    /// windows among its children) and the chrome's, with their actions,
    /// then the services'.
    #[must_use]
    pub fn resources(&self) -> Vec<ResourceSpec> {
        self.shared.resources()
    }

    /// What the service publishing `resource` reads of it now, as JSON;
    /// `None` when no service publishes it.
    #[must_use]
    pub fn read(&self, resource: &str) -> Option<serde_json::Value> {
        self.shared.services.borrow().read(resource)
    }

    /// Wait until every device call the services started has landed, and
    /// take them in: what a headless run does before it reads.
    pub fn settle_services(&self) {
        self.shared.services.borrow_mut().settle();
    }

    /// A window was at `place`: where search's Continue reopens it.
    pub fn record_place(&self, place: tilo_desktop_search::providers::places::Place) {
        self.shared.search.record_place(place);
    }

    /// Invoke the published action `action` of resource `resource`, as
    /// any caller of the action protocol does, and carry out what it asked
    /// of the session. A service's action that runs on the device is
    /// accepted at once; its result lands in the service's reading.
    ///
    /// # Errors
    /// The door's refusal: no such resource or action, bad arguments, or
    /// one the device cannot do now.
    pub fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ResourceOperationStart, DoorError> {
        let started = self.shared.invoke(resource, action, arguments)?;
        self.shared.follow_asks();
        Ok(started)
    }

    /// Invoke a service's action the person confirmed (restart, power
    /// off): one [`Session::invoke`] refused with
    /// [`DoorError::ConfirmationRequired`].
    ///
    /// # Errors
    /// The door's refusal: no such resource or action, bad arguments, or
    /// one the device cannot do now.
    pub fn invoke_confirmed(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ResourceOperationStart, DoorError> {
        match self
            .shared
            .services
            .borrow_mut()
            .invoke_confirmed(resource, action, arguments)
        {
            Some(Ok(Started::Done(output))) => Ok(ResourceOperationStart::Completed(output)),
            Some(Ok(Started::Accepted)) => Ok(ResourceOperationStart::Accepted),
            Some(Err(error)) => Err(error),
            None => Err(DoorError::Unknown(ActionAddress::new(resource, action))),
        }
    }

    /// The labels of the desktop's accessibility trees, the desk's and the
    /// chrome's, as assistive technology reads them.
    #[must_use]
    pub fn accessible_labels(&self) -> Vec<String> {
        self.shared
            .layers
            .each(|host| {
                host.a11y_nodes()
                    .into_iter()
                    .filter_map(|(_, node)| node.label().map(str::to_owned))
                    .collect::<Vec<_>>()
            })
            .concat()
    }

    /// Whether quick settings is open.
    #[must_use]
    pub fn quick_settings_open(&self) -> bool {
        self.shared
            .quick_settings
            .as_ref()
            .is_some_and(|open| open.get())
    }

    /// One turn of the loop: wait up to `timeout` for clients and devices,
    /// carry out what the desktop asked, and draw.
    ///
    /// # Errors
    /// The compositor failed.
    pub fn step(&mut self, timeout: std::time::Duration) -> Result<(), Error> {
        self.compositor.dispatch(timeout)?;
        self.shared.services.borrow_mut().poll();
        self.shared.follow_asks();
        self.compositor.render()?;
        self.shared.follow_asks();
        self.shared.observe();
        Ok(())
    }

    /// Run until `stop` holds.
    ///
    /// # Errors
    /// The compositor failed.
    pub fn run(&mut self, mut stop: impl FnMut(&Compositor) -> bool) -> Result<(), Error> {
        let shared = Rc::clone(&self.shared);
        self.compositor.run(|compositor| {
            shared.services.borrow_mut().poll();
            shared.follow_asks();
            shared.observe();
            stop(compositor)
        })
    }
}
