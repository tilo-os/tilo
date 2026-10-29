//! The native Tilo session: a thin host, as the web page is one.
//!
//! The compositor ([`tilo_compositor`]) shows buffers and routes input. The
//! session gives it the desktop every host shows
//! ([`tilo_desktop::desktop_host`]): the desk (backdrop, launchpad, the
//! strips and window bars of the desktop surface) under the windows, and
//! the chrome (top bar, strip ribbon and rail, overview, menus) above them;
//! the window policy of the desk's strips ([`tilo_surface_host::desktop`]),
//! so each window is a tab of a strip; and the desktop's surface apps, each
//! an ordinary app hosted on a layer of its own: the notification banners,
//! quick settings, and setup over everything while a person first uses the
//! device. What the chrome asks of its host comes back as [`DesktopAsk`]s,
//! carried out here.
//!
//! Everything a person can do here is a published action: the desk's and
//! the chrome's, and each surface app's through its door
//! ([`ui::device_local_door::SharedDoor`]). [`Session::resources`] lists
//! them and [`Session::invoke`] runs one, as any caller of the action
//! protocol on this device does.

#![cfg(target_os = "linux")]

mod chrome;
mod panels;
mod setup;
mod sheets;

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use tilo_actions::{ActionAddress, DoorError, ResourceSpec};
use tilo_compositor::{Compositor, DisplayConfig, Error, OutputConfig, WindowPolicy};
use tilo_desktop::about::{AboutLinks, AboutVariant, TiloMenu};
use tilo_desktop::chrome::ChromeIntent;
use tilo_desktop::desktop_host::{DeskLayer, DesktopAsk, DesktopChrome};
use tilo_desktop::launchpad::{CollaborationLaunchpad, LaunchpadAction};
use tilo_desktop::topbar::TopBarChrome;
use tilo_desktop_surface::DesktopHandle;
use tilo_desktop_surface::navigation::Command;
use tilo_quick_settings::QsEffect;
use tilo_surface_host::{DesktopHost, Placement, WidgetHost, desktop_with};
use ui::application::ComponentRunnerInputs;
use ui::headless::FrameHarness;
use ui::host_operation::ResourceOperationStart;

pub use tilo_quick_settings::device::{DeviceHost, SessionRequest, Wake};
pub use tilo_quick_settings::host::PanelHost;

use chrome::{SessionChrome, SessionRules};
use panels::{Banners, Held, Popover, Served};
use setup::Setup;
use sheets::Sheet;
use tilo_sheets::Seat as _;
use tilo_sheets::portal::PortalSeat;
use tilo_sheets::power::{PowerEffect, PowerSeat};
use tilo_sheets::workspace_settings::{
    Settings as WorkspaceSettings, WorkspaceSettingsEffect, WorkspaceSettingsSeat,
};

pub use os_actions::SessionAction;

pub use tilo_onboarding_host::view::OnboardingView;

/// The generation the session's own desktop commands carry.
const GENERATION: &str = "session";

/// The workspace menu's action that opens workspace settings.
const WORKSPACE_SETTINGS: &str = "workspace-settings";

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
    /// without it, the bar's status cluster opens nothing.
    pub quick_settings: Option<QuickSettings>,
    /// Setup to run over everything before the desktop is used: the first
    /// use of this device, or the recovery of its identity.
    pub setup: Option<OnboardingView>,
    /// Carries out what the person chose in the power menu (log out,
    /// suspend, restart, shut down, switch user); the session locks the
    /// screen itself. Without one, the choice is only logged.
    pub session_actions: fn(SessionAction) -> Result<(), String>,
}

/// Where quick settings reads the device from.
pub enum QuickSettings {
    /// The device host of this machine (or of a stand-in one), made with
    /// the session loop's wake: its readings land on the loop, what the
    /// panel asks of the desktop comes back to the session.
    Device(Box<dyn FnOnce(Wake) -> DeviceHost>),
    /// A host lent as it is: a screenshot's stand-in readings.
    Lent(Rc<dyn PanelHost>),
}

/// The settings app, which quick settings opens.
const SETTINGS_APP: &str = "so.tilo.settings";

/// The default for [`Options::session_actions`]: a session that owns no
/// machine (a test, a screenshot) carries out nothing.
#[allow(
    clippy::unnecessary_wraps,
    reason = "the shape every session action runner has"
)]
fn no_session_actions(action: SessionAction) -> Result<(), String> {
    tracing::info!(?action, "this session carries out no session actions");
    Ok(())
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
            setup: None,
            session_actions: no_session_actions,
        }
    }
}

/// What the session keeps between frames, shared with the loop.
struct Shared {
    handle: DesktopHandle,
    /// What the desk and the chrome asked of the session.
    asks: Rc<RefCell<Vec<DesktopAsk>>>,
    /// Where the chrome takes intents the session raises.
    intents: Rc<RefCell<Vec<ChromeIntent>>>,
    /// Whether quick settings is open, when the session has it.
    quick_settings: Option<Rc<Cell<bool>>>,
    /// The device behind quick settings, when the session reads one.
    device: Option<DeviceHost>,
    /// The power menu, workspace settings, and the questions the session
    /// asks for apps and services, by their seats.
    power: Option<PowerSeat>,
    workspace_settings: Option<WorkspaceSettingsSeat>,
    portal: Option<PortalSeat>,
    session_actions: fn(SessionAction) -> Result<(), String>,
    /// The power menu asked to lock the screen.
    lock: Cell<bool>,
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
                DesktopAsk::HostAction(action) if action == WORKSPACE_SETTINGS => {
                    self.open_workspace_settings();
                }
                other => tracing::debug!(?other, "desktop ask the session has no part in yet"),
            }
        }
        self.follow_device();
        self.follow_sheets();
    }

    /// Land what the device host read and carry out what the panel asked
    /// of the desktop.
    fn follow_device(&self) {
        let Some(device) = &self.device else {
            return;
        };
        device.set_open(self.quick_settings.as_ref().is_some_and(|open| open.get()));
        device.pump();
        for request in device.take_requests() {
            match request {
                SessionRequest::Effect(QsEffect::PowerMenu) => {
                    if let Some(open) = &self.quick_settings {
                        open.set(false);
                    }
                    if let Some(power) = &self.power {
                        power.presentation().open(false);
                    }
                }
                SessionRequest::Effect(QsEffect::LockScreen) => {
                    if let Some(open) = &self.quick_settings {
                        open.set(false);
                    }
                    self.lock.set(true);
                }
                SessionRequest::Effect(QsEffect::OpenSettings { .. }) => {
                    if let Some(open) = &self.quick_settings {
                        open.set(false);
                    }
                    let launch = Command::Launch {
                        generation: GENERATION.into(),
                        app: SETTINGS_APP.into(),
                    };
                    if let Err(error) = self.handle.dispatch(&launch, GENERATION) {
                        tracing::warn!(%error, "open settings");
                    }
                }
                other => {
                    tracing::debug!(?other, "quick settings asks what the session cannot do yet");
                }
            }
        }
    }

    /// Show workspace settings for the workspace in front.
    fn open_workspace_settings(&self) {
        let Some(seat) = &self.workspace_settings else {
            return;
        };
        let name = self.handle.read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.name.clone())
                .unwrap_or_default()
        });
        seat.show(WorkspaceSettings {
            name,
            ..seat.settings()
        });
        seat.reset();
        seat.presentation().open(false);
    }

    /// Carry out what the person chose in the sheets.
    fn follow_sheets(&self) {
        if let Some(power) = &self.power {
            for effect in power.take_asked() {
                power.presentation().close();
                let action = match effect {
                    PowerEffect::Lock => {
                        self.lock.set(true);
                        continue;
                    }
                    PowerEffect::LogOut => SessionAction::Logout,
                    PowerEffect::SwitchUser => SessionAction::SwitchUser,
                    PowerEffect::Suspend => SessionAction::Suspend,
                    PowerEffect::Restart => SessionAction::Restart,
                    PowerEffect::ShutDown => SessionAction::ShutDown,
                };
                if let Err(error) = (self.session_actions)(action) {
                    tracing::warn!(%error, ?action, "power menu");
                }
            }
        }
        if let Some(settings) = &self.workspace_settings {
            for effect in settings.take_asked() {
                match effect {
                    WorkspaceSettingsEffect::Rename { name } => self
                        .intents
                        .borrow_mut()
                        .push(ChromeIntent::RenameWorkspace { name }),
                    WorkspaceSettingsEffect::Close => {}
                    other => tracing::debug!(
                        ?other,
                        "workspace settings asks what the session cannot do yet"
                    ),
                }
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

/// What mounts the session's surface apps, each on a layer of its own
/// with its actions among the session's.
struct Mount<'a> {
    compositor: &'a mut Compositor,
    surfaces: Vec<Rc<RefCell<dyn Served>>>,
    /// This device and the person at it.
    who: (String, String),
    presentation: fn() -> ComponentRunnerInputs,
    scale: f64,
}

impl Mount<'_> {
    /// Host `surface` above what was mounted before, and publish its
    /// actions.
    fn add<T: tilo_compositor::SurfaceHost + Served + 'static>(
        &mut self,
        surface: T,
    ) -> Rc<RefCell<T>> {
        let held = Rc::new(RefCell::new(surface));
        self.compositor.add_host(Box::new(Held(Rc::clone(&held))));
        self.surfaces
            .push(Rc::clone(&held) as Rc<RefCell<dyn Served>>);
        held
    }

    /// The notification banners; their link, mounted or not.
    fn banners(&mut self) -> tilo_notification_banners::Link {
        let link = tilo_notification_banners::Link::new();
        match tilo_notification_banners::prepare(link.clone(), &self.who.0, &self.who.1)
            .and_then(|component| ui::App::from_component(component, (self.presentation)()))
        {
            Ok(app) => {
                self.add(Banners::new(app, link.clone(), self.scale));
            }
            Err(error) => tracing::warn!(%error, "notification banners not mounted"),
        }
        link
    }

    /// Quick settings, reading the device as `reads` says: the device
    /// host it reads, when the session drives one, and whether it is
    /// open, when it mounted.
    fn quick_settings(
        &mut self,
        reads: QuickSettings,
    ) -> (Option<DeviceHost>, Option<Rc<Cell<bool>>>) {
        let (device, panel) = match reads {
            QuickSettings::Device(make) => {
                let waker = self.compositor.waker();
                let device = make(Arc::new(move || waker.wake_by_ref()));
                let panel = device.panel();
                (Some(device), panel)
            }
            QuickSettings::Lent(panel) => (None, panel),
        };
        (device, self.panel(panel))
    }

    /// Quick settings' panel, reading the device through `panel`; whether
    /// it is open, when it mounted.
    fn panel(&mut self, panel: Rc<dyn PanelHost>) -> Option<Rc<Cell<bool>>> {
        let open = Rc::new(Cell::new(false));
        match tilo_quick_settings::prepare(panel, &self.who.0, &self.who.1, 1)
            .and_then(|component| ui::App::from_component(component, (self.presentation)()))
        {
            Ok(app) => {
                self.add(Popover::new(app, Rc::clone(&open), self.scale));
                Some(open)
            }
            Err(error) => {
                tracing::warn!(%error, "quick settings not mounted");
                None
            }
        }
    }

    /// A sheet lent `seat`; the seat, when it mounted.
    fn sheet<S: tilo_sheets::Seat>(
        &mut self,
        definition: &'static ui::application::ApplicationDefinition,
        seat: S,
    ) -> Option<S> {
        match Sheet::mount(
            definition,
            seat,
            (&self.who.0, &self.who.1),
            (self.presentation)(),
            self.scale,
        ) {
            Ok(sheet) => {
                let seat = sheet.seat().clone();
                self.add(sheet);
                Some(seat)
            }
            Err(error) => {
                tracing::warn!(%error, app = definition.app_id(), "sheet not mounted");
                None
            }
        }
    }
}

/// A running session: the compositor and the desktop it hosts.
pub struct Session {
    compositor: Compositor,
    desktop: DesktopHost,
    /// The chrome above the windows.
    chrome: Rc<RefCell<WidgetHost>>,
    shared: Rc<Shared>,
    banners: tilo_notification_banners::Link,
    /// The surface apps whose actions the session publishes.
    surfaces: Vec<Rc<RefCell<dyn Served>>>,
    /// Setup, while it runs: it withholds the desktop's actions.
    setup: Option<Rc<RefCell<Setup>>>,
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

        let mut mount = Mount {
            compositor: &mut compositor,
            surfaces: Vec::new(),
            who: (options.device.clone(), options.person.clone()),
            presentation: options.presentation,
            scale: options.scale,
        };
        let banners = mount.banners();
        let (device, quick_settings) = options
            .quick_settings
            .map_or((None, None), |reads| mount.quick_settings(reads));

        // The sheets, over the desktop and its panels.
        let power = mount.sheet(tilo_sheets::power::definition(), PowerSeat::default());
        let workspace_settings = mount.sheet(
            tilo_sheets::workspace_settings::definition(),
            WorkspaceSettingsSeat::default(),
        );
        let portal = mount.sheet(tilo_sheets::portal::definition(), PortalSeat::default());

        // Setup last: above everything, and first to the keyboard.
        setup::lend(mount.compositor.waker(), &options.person);
        let setup = options
            .setup
            .map(|view| mount.add(Setup::new(view, options.scale)));
        let surfaces = mount.surfaces;

        let shared = Rc::new(Shared {
            handle,
            asks,
            intents,
            quick_settings,
            device,
            power,
            workspace_settings,
            portal,
            session_actions: options.session_actions,
            lock: Cell::new(false),
        });
        Ok(Self {
            compositor,
            desktop,
            chrome,
            shared,
            banners,
            surfaces,
            setup,
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
    pub const fn desktop(&self) -> &DesktopHost {
        &self.desktop
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

    /// `read` of the desk's hosted tree, then of the chrome's.
    fn each_layer<R>(&self, mut read: impl FnMut(&mut FrameHarness) -> R) -> [R; 2] {
        let desk = self.desktop.with_widgets(|host| read(host.host_mut()));
        let chrome = read(self.chrome.borrow_mut().host_mut());
        [desk, chrome]
    }

    /// Whether setup is up: it withholds the desktop until it is done.
    #[must_use]
    pub fn setup_up(&self) -> bool {
        self.setup.as_ref().is_some_and(|setup| setup.borrow().up())
    }

    /// The doors of the surface apps offering their actions now, each
    /// freshly published.
    fn doors(&self) -> Vec<(ui::device_local_door::SharedDoor, Rc<RefCell<dyn Served>>)> {
        self.surfaces
            .iter()
            .filter_map(|surface| {
                let mut served = surface.borrow_mut();
                served.offered().then(|| {
                    served.serve();
                    (served.door(), Rc::clone(surface))
                })
            })
            .collect()
    }

    /// Everything the session publishes now: the desk's resource (its
    /// windows among its children), the chrome's, and each surface app's,
    /// with their actions. While setup is up, only setup's.
    #[must_use]
    pub fn resources(&self) -> Vec<ResourceSpec> {
        let mut resources = if self.setup_up() {
            Vec::new()
        } else {
            self.each_layer(|host| host.resources()).concat()
        };
        for (door, _) in self.doors() {
            resources.extend(door.resources());
        }
        resources
    }

    /// Invoke the published action `action` of resource `resource`, as
    /// any caller of the action protocol does, and carry out what it asked
    /// of the session.
    ///
    /// # Errors
    /// The door's refusal: no such resource or action, or bad arguments.
    pub fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ResourceOperationStart, DoorError> {
        let address = ActionAddress::new(resource, action);
        let started = match self.invoke_desktop(&address, arguments) {
            Some(started) => started,
            None => self.invoke_surface(&address, arguments)?,
        }?;
        self.shared.follow_asks();
        self.follow_lock();
        Ok(started)
    }

    /// Invoke `address` on the desk or the chrome, when one publishes it
    /// and setup is not up.
    fn invoke_desktop(
        &self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Option<Result<ResourceOperationStart, DoorError>> {
        if self.setup_up() {
            return None;
        }
        let [desk, chrome] = self.each_layer(|host| {
            tilo_actions::door::resolve(&host.resources(), address)
                .is_some()
                .then(|| {
                    host.invoke_published(address.resource.as_str(), &address.action, arguments)
                })
        });
        desk.or(chrome)
    }

    /// Invoke `address` through the door of the surface app publishing it,
    /// served at once.
    fn invoke_surface(
        &self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Result<ResourceOperationStart, DoorError>, DoorError> {
        let (door, surface) = self
            .doors()
            .into_iter()
            .find(|(door, _)| tilo_actions::door::resolve(&door.resources(), address).is_some())
            .ok_or_else(|| DoorError::Unknown(address.clone()))?;
        let answer = door.invoke(address.clone(), arguments.clone());
        surface.borrow_mut().serve();
        let answered = answer.borrow_mut().take();
        answered.ok_or_else(|| DoorError::Unknown(address.clone()))
    }

    /// The labels of the desktop's accessibility trees, the desk's and the
    /// chrome's, as assistive technology reads them.
    #[must_use]
    pub fn accessible_labels(&self) -> Vec<String> {
        if let Some(setup) = self.setup.as_ref().filter(|setup| setup.borrow().up()) {
            return setup.borrow_mut().accessible_labels();
        }
        self.each_layer(|host| {
            host.a11y_nodes()
                .into_iter()
                .filter_map(|(_, node)| node.label().map(str::to_owned))
                .collect::<Vec<_>>()
        })
        .concat()
    }

    /// The power menu's seat: the session puts it up.
    #[must_use]
    pub fn power_menu(&self) -> Option<&PowerSeat> {
        self.shared.power.as_ref()
    }

    /// Workspace settings' seat: what it shows, kept current by whoever
    /// knows the workspace.
    #[must_use]
    pub fn workspace_settings(&self) -> Option<&WorkspaceSettingsSeat> {
        self.shared.workspace_settings.as_ref()
    }

    /// The portal sheet's seat: the portal, authentication and
    /// network-secret services ask through it and read the answers.
    #[must_use]
    pub fn portal(&self) -> Option<&PortalSeat> {
        self.shared.portal.as_ref()
    }

    /// Lock the screen when the power menu asked to.
    fn follow_lock(&mut self) {
        if self.shared.lock.replace(false)
            && let Err(error) = self.compositor.lock_now()
        {
            tracing::warn!(%error, "lock the screen");
        }
    }

    /// The verified browser projection a carried-over setup brought: the
    /// browser starts with it.
    #[must_use]
    pub fn browser_continuity(&self) -> Option<std::path::PathBuf> {
        setup::browser_continuity()
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
        self.shared.follow_asks();
        self.compositor.render()?;
        self.shared.follow_asks();
        self.follow_lock();
        Ok(())
    }

    /// Run until `stop` holds.
    ///
    /// # Errors
    /// The compositor failed.
    pub fn run(&mut self, mut stop: impl FnMut(&Compositor) -> bool) -> Result<(), Error> {
        let shared = Rc::clone(&self.shared);
        loop {
            let mut stopped = false;
            self.compositor.run(|compositor| {
                shared.follow_asks();
                // The loop lends no compositor to lock with: leave it to
                // lock, then go on.
                stopped = stop(compositor);
                stopped || shared.lock.get()
            })?;
            if stopped || !shared.lock.get() {
                return Ok(());
            }
            self.follow_lock();
        }
    }
}
