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
//!
//! The session also hosts the system services it was started with
//! ([`Options::services`]): connectivity, audio and the rest publish their
//! resources beside the desktop's, and [`Session::invoke`] reaches their
//! actions as it reaches the desktop's, by address.

#![cfg(target_os = "linux")]

pub mod capture;
mod chrome;
mod config;
mod controls;
pub mod desktop_services;
pub mod displays;
mod env;
mod keys;
mod lists;
mod panels;
mod search;
mod setup;
mod sheets;
pub mod unlock;
mod window_motion;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tilo_actions::{ActionAddress, DoorError, ResourceSpec};
use tilo_compositor::{Compositor, DisplayConfig, Error, OutputConfig, WindowPolicy};
use tilo_desktop::chrome::ChromeIntent;
use tilo_desktop::desktop_actions::{CHROME_RESOURCE, DESK_RESOURCE};
use tilo_desktop::desktop_host::{DeskLayer, DesktopAsk};
use tilo_desktop::launchpad::{CollaborationLaunchpad, LaunchpadAction};
use tilo_desktop::osd::{OSD_RESOURCE, Osd, OsdKind};
use tilo_desktop_surface::DesktopHandle;
use tilo_desktop_surface::navigation::Command;
use tilo_quick_settings::QsEffect;
use tilo_service::Started;
use tilo_surface_host::{DeskGesture, DesktopHost, Doors, Published, WidgetHost, desktop_with};
use ui::application::ComponentRunnerInputs;
use ui::host_operation::ResourceOperationStart;
use ui::shortcut_table::ShortcutAction;

pub use config::SessionConfig;
pub use controls::{
    DISPLAYS_RESOURCE, INPUT_RESOURCE, MAGNIFIER_RESOURCE, Magnify, SESSION_RESOURCE,
    SessionCommand,
};
pub use env::SessionEnvironment;
pub use keys::{Invocation, KEYBOARD_BACKLIGHT, MEDIA, MICROPHONE, QUICK_SETTINGS};
pub use lists::{AppLists, Lists};
pub use search::{AppActions, SearchOptions};
pub use tilo_quick_settings::device::{DeviceHost, SessionRequest, Wake};
pub use tilo_quick_settings::host::PanelHost;
pub use tilo_service::{Service, Services};

use controls::SessionControls;
use keys::{Invocations, SessionKeys};
use panels::{Banners, Held, Popover, Readout};
use search::SessionSearch;
use setup::Setup;
use sheets::Sheet;
use tilo_desktop_search::providers::places::Place;
use tilo_sheets::Seat as _;
use tilo_sheets::file_picker::FilePickerSeat;
use tilo_sheets::lock::LockSeat;
use tilo_sheets::portal::PortalSeat;
use tilo_sheets::power::{PowerEffect, PowerSeat};
use tilo_sheets::workspace_settings::{
    Settings as WorkspaceSettings, WorkspaceSettingsEffect, WorkspaceSettingsSeat,
};

pub use os_actions::SessionAction;

pub use tilo_onboarding_host::view::OnboardingView;

/// The generation the session's own desktop commands carry.
const GENERATION: &str = "session";

/// How soon after a press outside quick settings closed it the bar's own
/// toggle of it is that same press, not a new ask to open it.
const DISMISS_GRACE: Duration = Duration::from_millis(300);

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
    /// The person's settings: shortcuts, input, gestures.
    pub config: SessionConfig,
    /// The system services the session hosts: what they publish is the
    /// session's, beside the desktop's.
    pub services: Vec<Box<dyn Service>>,
    /// What the desktop's search reaches beyond the desktop.
    pub search: SearchOptions,
    /// The action host: what open apps publish and run, and where what is
    /// chosen in search runs by address. Without one, search finds the
    /// desktop's own and starts nothing.
    pub apps: Option<Rc<dyn AppActions>>,
    /// Setup to run over everything before the desktop is used: the first
    /// use of this device, or the recovery of its identity.
    pub setup: Option<OnboardingView>,
    /// Carries out what the person chose in the power menu (log out,
    /// suspend, restart, shut down, switch user); the session locks the
    /// screen itself. Without one, the choice is only logged.
    pub session_actions: fn(SessionAction) -> Result<(), String>,
    /// Whose session this is, as the system knows them: the user a
    /// password unlocks the lock screen for.
    pub user: String,
    /// Whether a password unlocks the session; without one nothing does.
    pub verifier: std::sync::Arc<dyn unlock::PasswordVerifier>,
    /// What the services were handed to wake the session's loop when
    /// something lands: the session binds it to its loop once started.
    pub wake: tilo_service::LoopWake,
    /// The screenshots the portal asks for, which the session takes.
    pub captures: Option<capture::Captures>,
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
            config: SessionConfig::default(),
            services: Vec::new(),
            search: SearchOptions::default(),
            apps: None,
            setup: None,
            session_actions: no_session_actions,
            user: String::new(),
            verifier: std::sync::Arc::new(unlock::Unavailable),
            wake: tilo_service::LoopWake::default(),
            captures: None,
        }
    }
}

/// Quick settings as the session shows it.
struct QuickSettingsPanel {
    open: Rc<Cell<bool>>,
    /// When a press outside last closed it.
    dismissed: Rc<Cell<Option<Instant>>>,
}

/// What the session keeps between frames, shared with the loop.
struct Shared {
    handle: DesktopHandle,
    desktop: DesktopHost,
    search: Rc<SessionSearch>,
    /// What the desk and the chrome asked of the session.
    asks: Rc<RefCell<Vec<DesktopAsk>>>,
    /// Where the chrome takes intents the session raises.
    intents: Rc<RefCell<Vec<ChromeIntent>>>,
    quick_settings: Option<QuickSettingsPanel>,
    /// Every hosted surface's and service's published side: the session's
    /// action host.
    doors: Doors,
    /// Published actions keys and gestures asked for, waiting for the door.
    invocations: Invocations,
    /// What the session's own resources asked of the compositor.
    commands: Rc<RefCell<Vec<SessionCommand>>>,
    /// Quick settings' readings of the device, which a readout shows.
    readings: Rc<RefCell<Option<tilo_quick_settings::host::Feed>>>,
    /// The system services, reached for their readings; their actions go
    /// through the door like every other.
    services: Rc<RefCell<Services>>,
    /// The device behind quick settings, when the session reads one.
    device: Option<DeviceHost>,
    /// The power menu, workspace settings, and the questions the session
    /// asks for apps and services, by their seats.
    power: Option<PowerSeat>,
    workspace_settings: Option<WorkspaceSettingsSeat>,
    portal: Option<PortalSeat>,
    file_picker: Option<FilePickerSeat>,
    /// The lock screen, while the session is locked.
    lock: Option<LockSeat>,
    session_actions: fn(SessionAction) -> Result<(), String>,
    /// Where everything the session publishes stood when search last read
    /// it.
    observed: RefCell<Option<Vec<(usize, u64)>>>,
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
                    if let Some(panel) = &self.quick_settings {
                        let dismissed = panel
                            .dismissed
                            .take()
                            .is_some_and(|at| at.elapsed() < DISMISS_GRACE);
                        if !dismissed {
                            panel.open.set(!panel.open.get());
                        }
                    }
                }
                DesktopAsk::Intent(ChromeIntent::ShowQuickSettings) => self.show_quick_settings(),
                DesktopAsk::Launchpad(action) => self.launchpad(action),
                DesktopAsk::Shortcut(action) => self.shortcut(action),
                DesktopAsk::HostAction(action) if action == WORKSPACE_SETTINGS => {
                    self.open_workspace_settings();
                }
                other => tracing::debug!(?other, "desktop ask the session has no part in yet"),
            }
        }
        self.follow_device();
        self.follow_sheets();
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
                self.doors.invoke(&own.address, &own.arguments).map(drop)
            });
        }
        // What the session's own actions asked of it.
        self.follow_asks();
    }

    /// Give search its kept history and what the session publishes.
    fn start_search(&self) {
        if let Some(kept) = self.services.borrow().read(search::HISTORY) {
            self.search.restore(&kept);
        }
        self.observe();
    }

    /// Hand search what the session publishes when it changed: read after
    /// a frame, as the desk cannot read its own tree while it opens search.
    fn observe(&self) {
        let now = self.doors.revision();
        if now.is_some() && *self.observed.borrow() == now {
            return;
        }
        self.observed.replace(now);
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
        self.search.observe(&self.doors.resources(), open_tabs);
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

    fn close_quick_settings(&self) {
        if let Some(panel) = &self.quick_settings {
            panel.open.set(false);
        }
    }

    /// Land what the device host read and carry out what the panel asked
    /// of the desktop.
    fn follow_device(&self) {
        let Some(device) = &self.device else {
            return;
        };
        device.set_open(
            self.quick_settings
                .as_ref()
                .is_some_and(|panel| panel.open.get()),
        );
        device.pump();
        for request in device.take_requests() {
            match request {
                SessionRequest::Effect(QsEffect::PowerMenu) => {
                    self.close_quick_settings();
                    if let Some(power) = &self.power {
                        power.presentation().open(false);
                    }
                }
                SessionRequest::Effect(QsEffect::LockScreen) => {
                    self.close_quick_settings();
                    self.commands.borrow_mut().push(SessionCommand::Lock);
                }
                SessionRequest::Effect(QsEffect::OpenSettings { .. }) => {
                    self.close_quick_settings();
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
                        self.commands.borrow_mut().push(SessionCommand::Lock);
                        continue;
                    }
                    PowerEffect::LogOut => {
                        self.commands.borrow_mut().push(SessionCommand::LogOut);
                        continue;
                    }
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

    fn show_quick_settings(&self) {
        if let Some(panel) = &self.quick_settings {
            panel.open.set(true);
        }
    }

    /// A desktop shortcut the chrome left to its host.
    fn shortcut(&self, action: ShortcutAction) {
        let mut invocations = self.invocations.borrow_mut();
        match action {
            ShortcutAction::QuickSettings => {
                if let Some(panel) = &self.quick_settings {
                    panel.open.set(!panel.open.get());
                }
            }
            ShortcutAction::Notifications => {
                self.show_quick_settings();
                invocations.push(Invocation::new(QUICK_SETTINGS, "show-notifications"));
            }
            ShortcutAction::ClipboardHistory => {
                self.show_quick_settings();
                invocations.push(Invocation::new(QUICK_SETTINGS, "paste"));
            }
            ShortcutAction::Search => {
                invocations.push(Invocation::new(DESK_RESOURCE, "open-search"));
            }
            ShortcutAction::Lock => self.commands.borrow_mut().push(SessionCommand::Lock),
            ShortcutAction::ToggleMagnifier => self
                .commands
                .borrow_mut()
                .push(SessionCommand::Magnifier(Magnify::Toggle)),
            ShortcutAction::MagnifierZoomIn => self
                .commands
                .borrow_mut()
                .push(SessionCommand::Magnifier(Magnify::ZoomIn)),
            ShortcutAction::MagnifierZoomOut => self
                .commands
                .borrow_mut()
                .push(SessionCommand::Magnifier(Magnify::ZoomOut)),
            other => tracing::debug!(?other, "shortcut the session has no part in yet"),
        }
    }

    /// What the touchpad gestures the desktop took asked, as the chrome's
    /// published actions.
    fn follow_gestures(&self) {
        let gestures = self.desktop.take_gestures();
        let mut invocations = self.invocations.borrow_mut();
        for gesture in gestures {
            invocations.push(match gesture {
                DeskGesture::StepWindow(steps) => Invocation::shortcut(if steps < 0 {
                    ShortcutAction::FocusPreviousWindow
                } else {
                    ShortcutAction::FocusNextWindow
                }),
                DeskGesture::StepStrip(steps) => {
                    Invocation::new(CHROME_RESOURCE, "step-strip").with("steps", steps)
                }
                DeskGesture::SwitchWorkspace(delta) => {
                    Invocation::new(CHROME_RESOURCE, "switch-workspace").with("delta", delta)
                }
                DeskGesture::Overview(open) => Invocation::new(
                    CHROME_RESOURCE,
                    if open {
                        "show-overview"
                    } else {
                        "hide-overview"
                    },
                ),
            });
        }
    }

    /// Run the published actions keys and gestures asked for, through the
    /// door every caller uses.
    fn run_invocations(&self) -> bool {
        let invocations = std::mem::take(&mut *self.invocations.borrow_mut());
        let ran = !invocations.is_empty();
        for invocation in invocations {
            match self
                .doors
                .invoke(&invocation.address, &invocation.arguments)
            {
                Ok(_) => {
                    if let Some(kind) = invocation.readout {
                        self.show_readout(self.reading(kind));
                    }
                }
                Err(error) => tracing::debug!(
                    ?error,
                    address = %invocation.address.encode(),
                    "nothing carried out a key's action"
                ),
            }
        }
        ran
    }

    /// The readout of `kind` as the device reads now.
    fn reading(&self, kind: OsdKind) -> Osd {
        let readings = self.readings.borrow();
        let state = readings
            .as_ref()
            .map(tilo_quick_settings::host::Feed::current);
        let (level, on) = match (kind, state) {
            (OsdKind::Volume, Some(state)) => (state.volume_level(), !state.volume_muted()),
            (OsdKind::Brightness, Some(state)) => (state.brightness, true),
            (OsdKind::AirplaneMode, Some(state)) => (0.0, state.radios.airplane_mode),
            _ => (0.0, true),
        };
        Osd { kind, level, on }
    }

    /// Show `osd` through the readout's published action.
    fn show_readout(&self, osd: Osd) {
        let Ok(serde_json::Value::Object(arguments)) = serde_json::to_value(osd) else {
            return;
        };
        let address = ActionAddress::new(OSD_RESOURCE, "show");
        if let Err(error) = self.doors.invoke(&address, &arguments) {
            tracing::debug!(?error, "no readout shown");
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
    shared: Shared,
    banners: tilo_notification_banners::Link,
    /// The input preferences the compositor runs with now.
    input: tilo_compositor::InputConfig,
    /// The person logged out: the loop ends.
    ended: bool,
    /// The magnifier's zoom level and the one its toggle turns on at.
    magnifier: (u8, u8),
    /// Setup, while it runs: it withholds the desktop's actions.
    setup: Option<Rc<RefCell<Setup>>>,
    /// Checking the passwords the lock screen was given.
    unlocking: Unlocking,
    /// The screenshots asked for.
    captures: Option<capture::Captures>,
    /// The windows opening and closing.
    windows: window_motion::WindowMotions,
}

/// The lock screen's passwords being checked, off the loop.
struct Unlocking {
    user: String,
    verifier: std::sync::Arc<dyn unlock::PasswordVerifier>,
    verdicts: (
        std::sync::mpsc::Sender<unlock::Verdict>,
        std::sync::mpsc::Receiver<unlock::Verdict>,
    ),
    /// Wakes the loop when a verdict lands.
    wake: std::task::Waker,
}

impl Session {
    /// A session on the compositor `start` makes with the desktop's window
    /// policy.
    ///
    /// # Errors
    /// The compositor did not start.
    pub fn new(
        mut options: Options,
        start: impl FnOnce(Box<dyn WindowPolicy>) -> Result<Compositor, Error>,
    ) -> Result<Self, Error> {
        let mut doors = Doors::default();
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
        desktop.set_gestures(options.config.gestures);
        let handle = desktop.with_policy(|policy| policy.handle().clone());
        let mut compositor = start(Box::new(policy))?;
        // What the services take in wakes the loop instead of waiting for
        // its next pass.
        options.wake.bind(compositor.waker());
        compositor.set_input_config(options.config.input.clone());
        compositor.add_host(Box::new(desktop.clone()));
        doors.add(Rc::new(RefCell::new(desktop.clone())));

        // The chrome above the windows: its own tree, beside the desk's.
        let intents = Rc::new(RefCell::new(Vec::new()));
        let chrome = Rc::new(RefCell::new(chrome::host(
            &handle,
            &options,
            Rc::clone(&intents),
            Rc::clone(&asks),
        )));
        compositor.add_host(Box::new(Held(Rc::clone(&chrome))));
        doors.add(chrome);

        let banners = mount_banners(&mut compositor, &mut doors, &options);

        let readings = Rc::new(RefCell::new(None));
        let (device, quick_settings) =
            mount_device(&mut compositor, &mut doors, &mut options, &readings);

        // The sheets, over the desktop and its panels.
        let Sheets {
            power,
            workspace_settings,
            portal,
            file_picker,
            lock,
        } = mount_sheets(&mut compositor, &mut doors, &options);

        let (commands, invocations) = mount_controls(
            &mut compositor,
            &mut doors,
            std::mem::take(&mut options.config.shortcuts),
            options.scale,
        );

        // Setup last.
        let setup = mount_setup(
            &mut compositor,
            &mut doors,
            options.setup.take(),
            &options.person,
            options.scale,
        );

        let services = mount_services(&mut doors, std::mem::take(&mut options.services));

        let search = Rc::new(SessionSearch::new(options.search, options.apps));
        handle.set_search_host(Rc::clone(&search) as Rc<dyn tilo_desktop_surface::SearchHost>);
        let shared = Shared {
            handle,
            desktop,
            search,
            asks,
            intents,
            quick_settings,
            doors,
            invocations,
            commands,
            readings,
            services,
            device,
            power,
            workspace_settings,
            portal,
            file_picker,
            lock,
            session_actions: options.session_actions,
            observed: RefCell::new(None),
            places_seen: RefCell::default(),
        };
        shared.start_search();
        let loop_wake = compositor.waker();
        Ok(Self {
            compositor,
            shared,
            banners,
            input: options.config.input,
            setup,
            unlocking: Unlocking {
                user: options.user,
                verifier: options.verifier,
                verdicts: std::sync::mpsc::channel(),
                wake: loop_wake,
            },
            captures: options.captures,
            windows: window_motion::WindowMotions::new((options.presentation)().theme.motion()),
            ended: false,
            magnifier: (
                settings_store::magnifier::OFF,
                settings_store::magnifier::DEFAULT_ZOOM,
            ),
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
        &self.shared.desktop
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

    /// Reach `surface` (a service the session runs in its process) through
    /// the session's door from now on: its resources are published beside
    /// the desktop's, and keys that ask for its actions reach it.
    pub fn publish(&mut self, surface: tilo_surface_host::Shared) {
        self.shared.doors.add(surface);
    }

    /// Everything the session publishes now: the desk's resource (its
    /// windows among its children), the chrome's, the surface apps', the
    /// session's own and the services', with their actions.
    #[must_use]
    pub fn resources(&self) -> Vec<ResourceSpec> {
        self.shared.doors.resources()
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
        let address = ActionAddress::new(resource, action);
        let started = self.shared.doors.invoke(&address, arguments)?;
        self.settle()
            .map_err(|error| DoorError::Failed(error.to_string()))?;
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

    /// The labels of what the session shows to assistive technology: the
    /// desk's, the chrome's and every open surface app's.
    #[must_use]
    pub fn accessible_labels(&self) -> Vec<String> {
        self.shared.doors.accessible_labels()
    }

    /// Whether the person logged out, which ends the session's loop.
    #[must_use]
    pub const fn ended(&self) -> bool {
        self.ended
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

    /// The file chooser's seat: the portal service asks it for files and
    /// reads the answer.
    #[must_use]
    pub fn file_picker(&self) -> Option<&FilePickerSeat> {
        self.shared.file_picker.as_ref()
    }

    /// Whether setup is up: it withholds the desktop until it is done.
    #[must_use]
    pub fn setup_up(&self) -> bool {
        self.setup.as_ref().is_some_and(|setup| setup.borrow().up())
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
            .is_some_and(|panel| panel.open.get())
    }

    /// The input preferences the compositor runs with now.
    #[must_use]
    pub const fn input_config(&self) -> &tilo_compositor::InputConfig {
        &self.input
    }

    /// Carry out everything asked since the last look: the desktop's asks,
    /// the gestures it took, the actions keys asked for, and what those
    /// asked in turn.
    fn settle(&mut self) -> Result<(), Error> {
        self.follow_lock()?;
        if let Some(captures) = &self.captures {
            captures.serve(&mut self.compositor);
        }
        // Each round can ask for another (a key's shortcut the chrome
        // leaves to the session); a few rounds settle any chain.
        for _ in 0..4 {
            self.shared.follow_asks();
            self.shared.follow_gestures();
            let ran = self.shared.run_invocations();
            let commands = std::mem::take(&mut *self.shared.commands.borrow_mut());
            let commanded = !commands.is_empty();
            for command in commands {
                self.command(command)?;
            }
            if !ran && !commanded && self.shared.asks.borrow().is_empty() {
                break;
            }
            // What ran may have queued desktop commands on the shared
            // handle (the chrome switching workspace): the desk carries
            // them out on its next pass.
            self.shared.desktop.with_widgets(WidgetHost::relayout);
        }
        Ok(())
    }

    /// Follow the session's lock: put the lock screen up when it locks
    /// and down when it unlocks, keep its clock current, check the
    /// passwords it was given off the loop, and lift the lock for one that
    /// unlocks. A suspend the idle timers asked for goes to the system.
    fn follow_lock(&mut self) -> Result<(), Error> {
        use tilo_compositor::SessionTransition;
        let lock = self.shared.lock.clone();
        for event in self.compositor.take_session_events() {
            match event {
                SessionTransition::Locked => {
                    if let Some(lock) = &lock {
                        lock.lifted();
                        lock.presentation().open(true);
                    }
                }
                SessionTransition::Unlocked => {
                    if let Some(lock) = &lock {
                        lock.lifted();
                        lock.presentation().close();
                        lock.presentation().gone();
                    }
                }
                SessionTransition::Suspend => {
                    if let Err(error) = (self.shared.session_actions)(SessionAction::Suspend) {
                        tracing::warn!(%error, "suspend");
                    }
                }
                _ => {}
            }
        }
        let Some(lock) = lock.filter(|lock| lock.presentation().is_open()) else {
            return Ok(());
        };
        lock.show(tilo_sheets::lock::Shown {
            clock: ui::clock::time_of_day(&ui::clock::local_now()),
            person: self.unlocking.user.clone(),
            ..lock.shown()
        });
        for password in lock.take_attempts() {
            let verifier = std::sync::Arc::clone(&self.unlocking.verifier);
            let user = self.unlocking.user.clone();
            let verdicts = self.unlocking.verdicts.0.clone();
            let wake = self.unlocking.wake.clone();
            let checking = std::thread::Builder::new()
                .name("tilo-unlock".into())
                .spawn(move || {
                    let _ = verdicts.send(verifier.verify(&user, &password));
                    wake.wake();
                });
            if let Err(error) = checking {
                tracing::warn!(%error, "the password could not be checked");
                lock.refuse(tilo_sheets::lock::unavailable());
            }
        }
        let mut unlocked = false;
        while let Ok(verdict) = self.unlocking.verdicts.1.try_recv() {
            match verdict {
                unlock::Verdict::Unlocks => {
                    self.compositor.unlock()?;
                    unlocked = true;
                }
                unlock::Verdict::Wrong => lock.refuse(tilo_sheets::lock::wrong_password()),
                unlock::Verdict::Unavailable => lock.refuse(tilo_sheets::lock::unavailable()),
            }
        }
        // An unlock lands as a transition: follow it at once.
        if unlocked {
            return self.follow_lock();
        }
        Ok(())
    }

    fn command(&mut self, command: SessionCommand) -> Result<(), Error> {
        match command {
            SessionCommand::Lock => self.compositor.lock_now(),
            SessionCommand::Displays { mode } => {
                let changes = displays::arrange(mode, &self.compositor.outputs());
                // A refused arrangement leaves the displays as they were.
                if !changes.is_empty()
                    && let Err(error) = self.compositor.configure_outputs(&changes)
                {
                    tracing::warn!(%error, ?mode, "the displays were not arranged");
                }
                Ok(())
            }
            SessionCommand::Magnifier(step) => {
                let (zoom, remembered) = self.magnifier;
                self.magnifier = settings_store::magnifier::step(zoom, remembered, step.into());
                self.compositor.set_zoom(f64::from(self.magnifier.0));
                Ok(())
            }
            SessionCommand::LogOut => {
                // The login wrapper tears the session down once this
                // process ends; without one, ending is the whole of it.
                if let Err(error) = os_actions::request_logout() {
                    tracing::info!(%error, "no login wrapper to tell of the logout");
                }
                self.ended = true;
                Ok(())
            }
            SessionCommand::SetTouchpad { on } => {
                let enabled = on.unwrap_or(!self.input.touchpad_enabled);
                self.input.touchpad_enabled = enabled;
                self.compositor.set_input_config(self.input.clone());
                self.shared.show_readout(Osd {
                    kind: OsdKind::Touchpad,
                    level: 0.0,
                    on: enabled,
                });
                Ok(())
            }
        }
    }

    /// One turn of the loop: wait up to `timeout` for clients and devices,
    /// carry out what the desktop asked, and draw.
    ///
    /// # Errors
    /// The compositor failed.
    pub fn step(&mut self, timeout: Duration) -> Result<(), Error> {
        self.compositor.dispatch(timeout)?;
        self.shared.services.borrow_mut().poll();
        self.settle()?;
        self.windows.step(&mut self.compositor, Instant::now());
        self.compositor.render()?;
        self.settle()?;
        self.shared.observe();
        Ok(())
    }

    /// Run until `stop` holds or the backend is done.
    ///
    /// # Errors
    /// The compositor failed.
    pub fn run(&mut self, mut stop: impl FnMut(&Compositor) -> bool) -> Result<(), Error> {
        while !self.ended && !self.compositor.closed() && !stop(&self.compositor) {
            let timeout = self.compositor.next_wait();
            self.step(timeout)?;
        }
        Ok(())
    }
}

/// Quick settings as a popover under the bar, lent `panel`'s readings
/// (which `readings` then holds too).
fn mount_quick_settings(
    compositor: &mut Compositor,
    doors: &mut Doors,
    panel: Rc<dyn PanelHost>,
    options: &Options,
    readings: &panels::Readings,
) -> Option<QuickSettingsPanel> {
    let open = Rc::new(Cell::new(false));
    let dismissed = Rc::new(Cell::new(None));
    let panel = Rc::new(panels::Tap {
        inner: panel,
        feed: Rc::clone(readings),
    });
    match tilo_quick_settings::prepare(panel, &options.device, &options.person, 1)
        .and_then(|component| ui::App::from_component(component, (options.presentation)()))
    {
        Ok(app) => {
            let surface = Rc::new(RefCell::new(Popover::new(
                app,
                Rc::clone(&open),
                Rc::clone(&dismissed),
                options.scale,
            )));
            compositor.add_host(Box::new(Held(Rc::clone(&surface))));
            doors.add(surface);
            Some(QuickSettingsPanel { open, dismissed })
        }
        Err(error) => {
            tracing::warn!(%error, "quick settings not mounted");
            None
        }
    }
}

/// The system services' published side, behind the session's door like
/// every hosted surface: one action host for all of them.
struct ServiceDoor(Rc<RefCell<Services>>);

impl Published for ServiceDoor {
    fn resources(&mut self) -> Vec<ResourceSpec> {
        self.0.borrow().resources()
    }

    fn revision(&self) -> Option<u64> {
        Some(self.0.borrow().revision())
    }

    fn invoke(
        &mut self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ResourceOperationStart, DoorError> {
        match self
            .0
            .borrow_mut()
            .invoke(&address.resource, &address.action, arguments)
        {
            Some(Ok(Started::Done(output))) => Ok(ResourceOperationStart::Completed(output)),
            Some(Ok(Started::Accepted)) => Ok(ResourceOperationStart::Accepted),
            Some(Err(error)) => Err(error),
            None => Err(DoorError::Unknown(address.clone())),
        }
    }

    fn accessible_labels(&mut self) -> Vec<String> {
        Vec::new()
    }
}

/// A sheet lent `seat`, hosted over the desktop and its panels with its
/// actions behind the door while it is up; the seat, when it mounted.
/// Setup, when the person has not been through it: above everything,
/// first to the keyboard, and while it is up the only surface offering
/// actions.
fn mount_setup(
    compositor: &mut Compositor,
    doors: &mut Doors,
    view: Option<OnboardingView>,
    person: &str,
    scale: f64,
) -> Option<Rc<RefCell<Setup>>> {
    setup::lend(compositor.waker(), person);
    view.map(|view| {
        let setup = Rc::new(RefCell::new(Setup::new(view, scale)));
        compositor.add_host(Box::new(Held(Rc::clone(&setup))));
        doors.add(Rc::clone(&setup) as tilo_surface_host::Shared);
        setup
    })
}

/// The hardware keys' readout above the apps, and the session's own
/// resources and the keys it answers wherever the keyboard is: above every
/// surface, so offered every press first.
fn mount_controls(
    compositor: &mut Compositor,
    doors: &mut Doors,
    shortcuts: ui::shortcut_table::ShortcutConfig,
    scale: f64,
) -> (Rc<RefCell<Vec<SessionCommand>>>, Invocations) {
    let readout = Rc::new(RefCell::new(Readout::new(scale)));
    compositor.add_host(Box::new(Held(Rc::clone(&readout))));
    doors.add(readout);
    let commands = Rc::new(RefCell::new(Vec::new()));
    doors.add(Rc::new(RefCell::new(SessionControls::new(Rc::clone(
        &commands,
    )))));
    let invocations = Invocations::default();
    compositor.add_host(Box::new(SessionKeys::new(
        shortcuts,
        Rc::clone(&invocations),
    )));
    (commands, invocations)
}

/// The services, published on the door.
fn mount_services(doors: &mut Doors, started: Vec<Box<dyn Service>>) -> Rc<RefCell<Services>> {
    let mut services = Services::default();
    for service in started {
        services.add(service);
    }
    let services = Rc::new(RefCell::new(services));
    doors.add(Rc::new(RefCell::new(ServiceDoor(Rc::clone(&services)))));
    services
}

/// The notification banners, above the windows.
fn mount_banners(
    compositor: &mut Compositor,
    doors: &mut Doors,
    options: &Options,
) -> tilo_notification_banners::Link {
    let banners = tilo_notification_banners::Link::new();
    match tilo_notification_banners::prepare(banners.clone(), &options.device, &options.person)
        .and_then(|component| ui::App::from_component(component, (options.presentation)()))
    {
        Ok(app) => {
            let surface = Rc::new(RefCell::new(Banners::new(
                app,
                banners.clone(),
                options.scale,
            )));
            compositor.add_host(Box::new(Held(Rc::clone(&surface))));
            doors.add(surface);
        }
        Err(error) => tracing::warn!(%error, "notification banners not mounted"),
    }
    banners
}

/// The device's own backend, when the session runs one, and the quick
/// settings panel over it.
fn mount_device(
    compositor: &mut Compositor,
    doors: &mut Doors,
    options: &mut Options,
    readings: &panels::Readings,
) -> (Option<DeviceHost>, Option<QuickSettingsPanel>) {
    let (device, panel) = match options.quick_settings.take() {
        Some(QuickSettings::Device(make)) => {
            let waker = compositor.waker();
            let device = make(Arc::new(move || waker.wake_by_ref()));
            let panel = device.panel();
            (Some(device), Some(panel))
        }
        Some(QuickSettings::Lent(panel)) => (None, Some(panel)),
        None => (None, None),
    };
    let quick_settings =
        panel.and_then(|panel| mount_quick_settings(compositor, doors, panel, options, readings));
    (device, quick_settings)
}

/// The sheets the session lends its seats to.
struct Sheets {
    power: Option<PowerSeat>,
    workspace_settings: Option<WorkspaceSettingsSeat>,
    portal: Option<PortalSeat>,
    file_picker: Option<FilePickerSeat>,
    lock: Option<LockSeat>,
}

fn mount_sheets(compositor: &mut Compositor, doors: &mut Doors, options: &Options) -> Sheets {
    let who = (options.device.as_str(), options.person.as_str());
    let power = mount_sheet(
        compositor,
        doors,
        tilo_sheets::power::definition(),
        PowerSeat::default(),
        who,
        options,
    );
    let workspace_settings = mount_sheet(
        compositor,
        doors,
        tilo_sheets::workspace_settings::definition(),
        WorkspaceSettingsSeat::default(),
        who,
        options,
    );
    let portal = mount_sheet(
        compositor,
        doors,
        tilo_sheets::portal::definition(),
        PortalSeat::default(),
        who,
        options,
    );
    let file_picker = mount_sheet(
        compositor,
        doors,
        tilo_sheets::file_picker::definition(),
        FilePickerSeat::default(),
        who,
        options,
    );
    let lock = mount_lock_screen(compositor, doors, who, options);
    Sheets {
        power,
        workspace_settings,
        portal,
        file_picker,
        lock,
    }
}

fn mount_sheet<S: tilo_sheets::Seat + 'static>(
    compositor: &mut Compositor,
    doors: &mut Doors,
    definition: &'static ui::application::ApplicationDefinition,
    seat: S,
    who: (&str, &str),
    options: &Options,
) -> Option<S> {
    match Sheet::mount(
        definition,
        seat,
        who,
        (options.presentation)(),
        options.scale,
    ) {
        Ok(sheet) => {
            let seat = sheet.seat().clone();
            let sheet = Rc::new(RefCell::new(sheet));
            compositor.add_host(Box::new(Held(Rc::clone(&sheet))));
            doors.add(sheet);
            Some(seat)
        }
        Err(error) => {
            tracing::warn!(%error, app = definition.app_id(), "sheet not mounted");
            None
        }
    }
}

/// The lock screen, hosted over nothing while the session is locked; its
/// seat, when it mounted.
fn mount_lock_screen(
    compositor: &mut Compositor,
    doors: &mut Doors,
    who: (&str, &str),
    options: &Options,
) -> Option<LockSeat> {
    match Sheet::mount(
        tilo_sheets::lock::definition(),
        LockSeat::default(),
        who,
        (options.presentation)(),
        options.scale,
    ) {
        Ok(sheet) => {
            let seat = sheet.seat().clone();
            let sheet = Rc::new(RefCell::new(sheet.locking()));
            compositor.add_host(Box::new(Held(Rc::clone(&sheet))));
            doors.add(sheet);
            Some(seat)
        }
        Err(error) => {
            tracing::warn!(%error, "lock screen not mounted");
            None
        }
    }
}
