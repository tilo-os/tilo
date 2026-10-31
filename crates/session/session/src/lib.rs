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

mod a11y_settings;
mod activity;
mod activity_provider;
pub mod agent_door;
mod app_icons;
mod app_sheets;
mod automation;
mod bars;
pub mod capture;
mod capture_send;
mod capture_studio;
mod chrome;
mod config;
pub mod controller_nav;
pub mod controllers;
mod controls;
mod desk;
mod desk_history;
mod desk_layout;
mod desk_resource;
mod desk_restore;
pub mod desktop_services;
mod dictation;
mod directory;
mod display_confirm;
mod display_settings;
mod display_sheet;
pub mod displays;
mod downloaded;
mod emoji;
mod engine;
mod env;
pub mod fabric;
mod held;
mod here;
mod i18n;
pub mod identity;
mod kept;
mod keys;
mod launches;
mod lists;
mod lock_screen;
pub mod mount;
pub mod notices;
mod notifications;
mod owner_session;
mod panels;
mod people;
mod plane;
pub mod portal_link;
mod projection;
mod projection_video;
mod questions;
mod records;
mod rotation;
pub mod run;
mod search;
mod setup;
mod sheets;
pub mod shell;
mod shortcut_guide;
pub mod surfaces;
pub mod sync;
mod system_sounds;
pub mod unlock;
mod wallpapers;
mod window_motion;
mod workspace_settings;
mod workspaces;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tilo_actions::{ActionAddress, DoorError, ResourceSpec};
use tilo_compositor::{Compositor, DisplayConfig, Error, NestedConfig, OutputConfig, WindowPolicy};
use tilo_desktop::chrome::ChromeIntent;
use tilo_desktop::desktop_actions::{CHROME_RESOURCE, DESK_RESOURCE};
use tilo_desktop::desktop_host::{
    DesktopAsk, JOIN_WORKSPACE, LEAVE_WORKSPACE, NEW_WORKSPACE, WORKSPACE_SETTINGS,
};
use tilo_desktop::osd::{Osd, OsdKind};
use tilo_desktop_surface::DesktopHandle;
use tilo_lock::MutexRecover as _;
use tilo_service::Started;
use tilo_surface_host::{DesktopHost, Doors, WidgetHost};
use ui::application::ComponentRunnerInputs;
use ui::host_operation::ResourceOperationStart;
use ui::shortcut_table::ShortcutAction;

pub use automation::FinishedShortcutRun;
pub use bars::BarShown;
pub use config::SessionConfig;
pub use controls::{
    CLIPBOARD_RESOURCE, DISPLAYS_RESOURCE, INPUT_RESOURCE, MAGNIFIER_RESOURCE, Magnify,
    NIGHT_LIGHT_RESOURCE, SESSION_RESOURCE, SessionCommand,
};
pub use engine::Person;
pub use env::SessionEnvironment;
pub use keys::{Invocation, KEYBOARD_BACKLIGHT, MEDIA, MICROPHONE, QUICK_SETTINGS};
pub use launches::Launches;
pub use lists::{AppLists, Lists};
pub use mount::vpn::KillSwitch;
pub use mount::wake::{Seams as WakeSeams, WakeInterfaces};
pub use records::Records;
pub use rotation::Accelerometer;
pub use run::{Backend, Reach};
pub use search::{AppActions, SearchOptions};
pub use tilo_quick_settings::device::{DeviceHost, SessionRequest, Wake};
pub use tilo_quick_settings::host::PanelHost;
pub use tilo_service::{Service, Services};

use keys::{GuideKeys, Invocations};
use mount::panels::{KeyContext, mount_controls, mount_device, mount_setup};
use mount::services::{mount_services, start_action_plane};
use panels::{Banners, Held};
use plane::Plane;
use search::SessionSearch;
use setup::Setup;
use tilo_app_launch::Doors as WindowDoors;
use tilo_sheets::Seat as _;
use tilo_sheets::directory::DirectorySeat;
use tilo_sheets::display_confirm::ConfirmSeat;
use tilo_sheets::file_picker::FilePickerSeat;
use tilo_sheets::lock::LockSeat;
use tilo_sheets::portal::PortalSeat;
use tilo_sheets::power::{PowerEffect, PowerSeat};
use tilo_sheets::shortcut_guide::GuideSeat;
use tilo_sheets::uninstall::UninstallSeat;
use tilo_sheets::workspace_settings::WorkspaceSettingsSeat;

pub use os_actions::SessionAction;

pub use tilo_onboarding_host::view::OnboardingView;

/// The generation the session's own desktop commands carry.
const GENERATION: &str = "session";

/// How soon after a press outside quick settings closed it the bar's own
/// toggle of it is that same press, not a new ask to open it.
const DISMISS_GRACE: Duration = Duration::from_millis(300);

/// How long a locked session may show no lock screen before the session puts
/// it up again: the frame that follows putting it up is drawn in the same
/// turn, so nothing of it for a moment is nothing of it at all.
const LOCK_SCREEN_GRACE: Duration = Duration::from_millis(500);

/// How many times the session puts the lock screen up within one lock before
/// it lifts the lock. Putting it up is the same path every time — the sheet
/// the session has, shown again, or a sheet mounted as it was at the start —
/// so three tries cover a screen that went away once or twice, and a screen
/// that needs more than that is not staying up at all: the machine is better
/// unlocked than locked behind it.
const LOCK_SCREEN_TRIES: u32 = 3;

/// How often the desk is written while it changes.
const KEEP_DESK_EVERY: Duration = Duration::from_secs(2);

/// How warm the night light makes the displays, in kelvin.
const NIGHT_LIGHT_WARMTH: u32 = 3_400;

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
    /// The tilolang shell package's bar (`apps/tl/shell`) is shown as a layer over the
    /// desktop. Off until it covers every display and steps aside for a fullscreen window as
    /// the desktop's own bar does.
    pub shell_bar: bool,
    /// What quick settings reads of the device and carries out on it;
    /// without it, the bar's status cluster opens nothing.
    pub quick_settings: Option<QuickSettings>,
    /// The controllers in the room, which drive the desktop; without one, a
    /// controller does nothing here.
    pub controllers: Option<controllers::Source>,
    /// The person's settings: shortcuts, input, gestures.
    pub config: SessionConfig,
    /// The built-in panel turns with the way up this reads the device.
    pub accelerometer: Accelerometer,
    /// The system services the session hosts: what they publish is the
    /// session's, beside the desktop's.
    pub services: Vec<Box<dyn Service>>,
    /// What the desktop's search reaches beyond the desktop.
    pub search: SearchOptions,
    /// The action host: what open apps publish and run, and where what is
    /// chosen in search runs by address. Without one, search finds the
    /// desktop's own and starts nothing.
    pub apps: Option<Rc<dyn AppActions>>,
    /// The apps this session starts: what the desktop asks for, by name, and
    /// what a search row or a file opens.
    pub launch: Option<Rc<mount::apps::Apps>>,
    /// The apps this session started, which its broker admits a connection
    /// for: the app, room and process it launched itself. The apps it starts
    /// record their launch here; a session whose apps run in its own process
    /// (a test) records one itself.
    pub launches: std::sync::Arc<Launches>,
    /// The drives the person plugs in, which this session announces and acts
    /// on when they open or eject one.
    pub drives: Option<Rc<mount::drives::Drives>>,
    /// Setup to run over everything before the desktop is used: the first
    /// use of this device, or the recovery of its identity.
    pub setup: Option<OnboardingView>,
    /// Carries out what the person chose in the power menu (log out,
    /// suspend, restart, shut down, switch user); the session locks the
    /// screen itself. Without one, the choice is only logged.
    pub session_actions: fn(SessionAction) -> Result<(), String>,
    /// Whether the lock screen may offer suspend, restart and shut down:
    /// a session that owns the machine, not one running inside a window.
    pub lock_power: bool,
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
    /// Where the wireless display service answers: its control socket.
    /// `None`: the service's own, which the session's first request
    /// activates.
    pub projector: Option<std::path::PathBuf>,
    /// The folder the capture studio keeps its Screenshots folder in;
    /// `None`: the person's Pictures folder, else their home.
    pub pictures: Option<std::path::PathBuf>,
    /// The desktop services the session runs beside: what apps sent their
    /// notifications daemon, the questions they ask, and the portal they
    /// serve. The session feeds it what it knows of itself: which apps run
    /// and how, when the screen locks, which apps exited and what its
    /// workspaces grant.
    pub desktop: Option<Rc<desktop_services::DesktopServices>>,
    /// The questions the session's sheets answer, when nothing runs beside
    /// it to ask them: a session with desk services asks theirs.
    pub questions: Option<tilo_service::Questions>,
    /// The portal's link to the loop: what shares the session's screens,
    /// drives its seat and passes its selection.
    pub portal: Option<portal_link::SessionLink>,
    /// The surface apps the session runs as its own processes, each a client
    /// of its compositor.
    pub surfaces: Vec<surfaces::Surface>,
    /// It runs in a window of a desktop it did not start: that desktop owns
    /// idle, blanking, locking and suspend, so this one runs none of them.
    pub hosted: bool,
    /// The machine's power: the policy the person's settings resolved, the
    /// service over logind that acts on it and what holds it awake. Without
    /// one, this session runs no idle, lid, key or battery policy — it is a
    /// session in a window of another desktop, or a test's.
    pub power: Option<tilo_session_power::PowerLink>,
    /// Where this device keeps what outlives a session: the desk as it was
    /// left, and the pictures on it. Without one, the session starts on a
    /// fresh desk and keeps nothing (tests and screenshots).
    pub state: Option<std::path::PathBuf>,
    /// Whether the session takes part in the collaboration plane: who is
    /// here in each workspace's room, over Iroh. Off, this desktop has no
    /// collaborators but its own person.
    pub collab: bool,
    /// A collaboration plane to take part in instead of starting one: a
    /// session a test hands the presence a room would have carried
    /// ([`sync::Client::untethered`]), which says nothing on a network.
    pub sync: Option<Rc<sync::Client>>,
    /// The person's device fabric, when this session runs on one: what was
    /// copied here is shared with their other devices through it, and what
    /// they copied there joins the clipboard here.
    pub fabric: Option<tilo_device_fabric::link::SharedLink>,
    pub wake_seams: mount::wake::Seams,
    /// The person's VPN profile kill switches, when this session runs on a
    /// machine with the helper: a profile set to connect and not connected
    /// blocks traffic while its switch is on. Without one, nothing does.
    pub kill_switch: Option<KillSwitch>,
    /// Whether the apps this session runs read and invoke what its services
    /// publish, and on which socket. Off, the apps it starts reach no
    /// service at all.
    pub action_plane: ActionPlane,
    /// Where the session's agent door listens
    /// ([`agent_door::path`]): the socket a program of this login asks on to
    /// work in a workspace as an agent, and the person admits it. `None`:
    /// this session serves no agents (a test, or a session in a window of
    /// another desktop).
    pub agent_door: Option<std::path::PathBuf>,
    /// The runs of what people ask this session's agents to do. Without one
    /// the session keeps its own, which its broker delivers to the agents
    /// admitted here; one given is a run channel a test plays the agent's
    /// side of.
    pub agent_runs: Option<std::sync::Arc<tilo_agent_runs::AgentRuns>>,
}

/// Where a session serves its apps the resources its services publish.
#[derive(Clone, Debug, Default)]
pub enum ActionPlane {
    /// Nowhere: the apps reach no service.
    #[default]
    Off,
    /// On the socket the launcher hands an app for its session
    /// (`launcher::services::collab_socket`), which is also what tells the
    /// launcher to bind one.
    Launcher,
    /// On `path`: a session started for a test, which leaves the machine's
    /// own socket alone.
    At(std::path::PathBuf),
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

/// The default for [`Options::session_actions`]: a session that owns no
/// machine (a test, a screenshot, a session in a window) carries out
/// nothing.
#[allow(
    clippy::unnecessary_wraps,
    reason = "the shape every session action runner has"
)]
pub fn no_session_actions(action: SessionAction) -> Result<(), String> {
    tracing::info!(?action, "this session carries out no session actions");
    Ok(())
}

/// Who is here: this session's membership of each workspace's room, over the
/// collaboration plane. `None` when the session takes no part in it, or the
/// device's endpoint did not bind: a desktop with no one else on it.
fn start_collaboration(
    options: &Options,
    media: fabric::MediaContinuity,
    engine: Option<std::sync::Arc<engine::Engine>>,
) -> Option<Rc<sync::Client>> {
    if let Some(sync) = &options.sync {
        return Some(Rc::clone(sync));
    }
    if !options.collab {
        return None;
    }
    let identity = sync::device_identity();
    let own = sync::OwnPresence {
        label: options.person.clone(),
        user: identity
            .as_ref()
            .map(|identity| hex::encode(identity.device_cert.user_public_key)),
        ..sync::OwnPresence::default()
    };
    match sync::Client::start(
        identity,
        engine,
        own,
        sync::configured_hosts(),
        media,
        options.wake.handle(),
    ) {
        Ok(sync) => Some(Rc::new(sync)),
        Err(error) => {
            tracing::warn!(%error, "the collaboration plane");
            None
        }
    }
}

/// This device's rooms, holding what every workspace of the desk shows and
/// what a join takes in, kept beside the desk. `None` when this computer has
/// no identity to hold them with, or keeps no state to keep them in.
fn open_engine(
    options: &Options,
    handle: &DesktopHandle,
) -> Option<std::sync::Arc<engine::Engine>> {
    let workspaces: Vec<_> = handle.read(|tree| {
        tree.workspaces
            .iter()
            .filter(|workspace| !workspace.room().is_nil())
            .map(|workspace| (workspace.id, workspace.room()))
            .collect()
    });
    match engine::Engine::for_this_device(options.state.as_deref(), workspaces) {
        Ok(engine) => Some(std::sync::Arc::new(engine)),
        Err(error) => {
            tracing::debug!(%error, "this session holds no rooms");
            None
        }
    }
}

impl Default for Options {
    fn default() -> Self {
        Self {
            salt: 1,
            scale: 1.0,
            device: "this-device".into(),
            person: "person".into(),
            presentation: ComponentRunnerInputs::from_native_process,
            shell_bar: false,
            sync: None,
            quick_settings: None,
            controllers: None,
            config: SessionConfig::default(),
            accelerometer: Accelerometer::default(),
            services: Vec::new(),
            search: SearchOptions::default(),
            apps: None,
            launch: None,
            setup: None,
            session_actions: no_session_actions,
            lock_power: false,
            user: String::new(),
            verifier: std::sync::Arc::new(unlock::Unavailable),
            wake: tilo_service::LoopWake::default(),
            captures: None,
            projector: None,
            pictures: None,
            desktop: None,
            questions: None,
            portal: None,
            surfaces: Vec::new(),
            hosted: false,
            power: None,
            state: None,
            collab: true,
            fabric: None,
            wake_seams: mount::wake::Seams::default(),
            kill_switch: None,
            action_plane: ActionPlane::default(),
            agent_door: None,
            agent_runs: None,
            launches: std::sync::Arc::default(),
            drives: None,
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
    /// The workspace's activity: the desk's changes, and the records its
    /// apps keep.
    activity: activity::Activity,
    /// Who sits here, as the desk's rules and the workspaces' governance
    /// name them.
    person: String,
    search: Rc<SessionSearch>,
    /// The apps the person opens, when this session starts them.
    launch: Option<Rc<mount::apps::Apps>>,
    /// The windows this device left open, asked for again one at a time into
    /// the tabs they were in.
    restore: RefCell<desk_restore::Restore>,
    /// The drives the person plugs in, when this session watches them.
    drives: Option<Rc<mount::drives::Drives>>,
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
    /// The runs of what people ask this session's agents to do, fed every
    /// pass who sits here and what the room in front admits.
    agent_runs: std::sync::Arc<tilo_agent_runs::AgentRuns>,
    /// It runs in a window of a desktop it did not start (see
    /// [`Options::hosted`]).
    hosted: bool,
    /// The device behind quick settings, when the session reads one.
    device: Option<DeviceHost>,
    /// The person's device fabric, while this session runs on one.
    fabric: Option<tilo_device_fabric::link::SharedLink>,
    /// The person's devices as the panel was last fed them.
    devices: mount::devices::Devices,
    /// The wakes this session takes part in.
    waking: Rc<mount::wake::Wake>,
    /// What plays, as the desk's music widget shows it: written from the
    /// device's reading every frame it is pumped.
    music: tilo_desktop::desk_widgets::DeskMusic,
    /// The power menu, workspace settings, and the questions the session
    /// asks for apps and services, by their seats.
    power: Option<PowerSeat>,
    workspace_settings: Option<WorkspaceSettingsSeat>,
    /// The desk revision workspace settings was last fed at: its panes are
    /// rebuilt when the desk changed under it.
    ws_settings_fed: Cell<Option<u64>>,
    /// The workspace's emoji, which workspace settings shows and changes
    /// through Chat's record of the room.
    workspace_emoji: workspace_settings::EmojiTab,
    portal: Option<PortalSeat>,
    file_picker: Option<FilePickerSeat>,
    /// The question about a display change, which stands on every display
    /// at once so it is seen even when the change blanked one of them.
    display_confirm: Option<ConfirmSeat>,
    /// The workspaces this computer may join or open.
    directory: Option<DirectorySeat>,
    /// What the sites this computer knows list as public workspaces, read
    /// on a worker of its own.
    directory_store: directory::Store,
    /// The lock screen, while the session is locked.
    lock: Option<LockSeat>,
    /// This device's side of the person's session on their devices: what it
    /// says of its own lock and what it has followed from theirs.
    owner_session: owner_session::SessionFollow,
    /// What mounted the lock screen, to mount it again when a lock finds it
    /// gone.
    lock_mount: mount::sheets::LockMount,
    /// The desktop's own notices: what the session tells the person, such as
    /// leaving a workspace.
    notices: tilo_desktop_surface::DesktopNotifications,
    /// The apps' live activities on the bar, which the clock shows and the
    /// controls pressed on it go back to the app through.
    clock: Rc<chrome::Clock>,
    /// The running applications' tray items, which the bar shows and whose
    /// clicks the session hands back to the applications.
    tray: Option<std::sync::Arc<tilo_tray_host::Tray>>,
    /// The desk as this device last wrote it, and when, so a desk that keeps
    /// changing (a drag, a rename) is written once in a while rather than
    /// every frame.
    kept: RefCell<Option<(String, Instant)>>,
    /// The desk as the history last recorded it: what changed since is one
    /// change of the workspace.
    recorded: RefCell<Option<serde_json::Value>>,
    /// Where the desk and its pictures are kept between sessions.
    state: Option<std::path::PathBuf>,
    /// The pictures the desk shows, by workspace.
    wallpapers: std::sync::Arc<wallpapers::Wallpapers>,
    /// Whether the lock screen offers suspend, restart and shut down.
    lock_power: bool,
    /// The keyboard layouts the person switches between, and which one is
    /// in use, as the lock screen shows them.
    lock_layouts: Vec<String>,
    lock_layout: Cell<usize>,
    session_actions: fn(SessionAction) -> Result<(), String>,
    /// Where everything the session publishes stood when search last read
    /// it.
    observed: RefCell<Option<Vec<(usize, u64)>>>,
    /// The title each window had when search last recorded where it was.
    places_seen: RefCell<std::collections::HashMap<u64, String>>,
    /// The daemon's notifications on the desktop.
    notifications: RefCell<notifications::SessionNotifications>,
    /// The notification centre as the quick settings panel was last fed it.
    notice_view: RefCell<Option<tilo_quick_settings::NotificationsView>>,
    /// What the services ask the person, on the sheets.
    asker: questions::Asker,
    /// Who is here: this session's membership of each workspace's room,
    /// when it takes part in the collaboration plane.
    sync: Option<Rc<sync::Client>>,
    /// This device's rooms: what every workspace of the desk shows, and what
    /// a join takes in.
    engine: Option<std::sync::Arc<engine::Engine>>,
    /// The person's own workspaces, as all their devices hold the list of
    /// them: `None` when this computer has no identity to keep it for.
    personal: Option<RefCell<workspaces::Personal>>,
    /// The copies the person can paste again, and what the Clipboard section
    /// was last fed of them.
    clipboard: mount::clipboard::Clipboard,
    /// The person's settings, kept in step with their other devices.
    settings: mount::settings_sync::SettingsSync,
    /// The overview's pictures and the clock they fly on, which its panel
    /// in the chrome draws with.
    overview: mount::overview::Overview,
    /// Dictation: the speech the person allows once per dictation, and the
    /// sheet they answer on.
    dictation: dictation::Dictation,
    /// The emoji picker's picks: written into the field the person was typing
    /// into, and where that field's caret stands for the card to open at.
    emoji: emoji::Emoji,
    /// The shortcut guide: the sheet naming every key the desktop answers,
    /// and the table it explains.
    shortcut_guide: shortcut_guide::ShortcutGuide,
    /// The chrome above the windows as the session hosts it: where its
    /// bars stand on the displays, read back for the tests that drive them.
    chrome: Rc<RefCell<WidgetHost>>,
    /// Who is here, as the bar's faces open it: the roster it shows and
    /// where everyone else's pointer is.
    people: Rc<RefCell<people::People>>,
    /// The monitors of the displays a fullscreen window owns, which the
    /// bars read.
    fullscreen: Rc<RefCell<Vec<String>>>,
    /// The controller the person last drove the desktop with: the guide
    /// leads with its buttons until a key of their own is pressed.
    controller: Rc<Cell<Option<ui::ControllerFamily>>>,
    /// Whether the keys being routed now are a controller's own, standing
    /// for its presses: those say nothing about a keyboard.
    controller_keys: Rc<Cell<bool>>,
    /// The questions about the person's apps: whether to add a program they
    /// downloaded to their apps, and whether to uninstall one.
    app_sheets: app_sheets::AppSheets,
    /// The agent door, when this session serves one: what a program of this
    /// login asks on, and the person's answers about it.
    agent_door: RefCell<Option<agent_door::Loop>>,
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
                // The bar's faces, the launcher's Happening now header and a
                // press on someone in a banner all open who is here.
                DesktopAsk::Intent(ChromeIntent::ToggleCollaborators) => {
                    self.people.borrow().toggle();
                }
                DesktopAsk::Intent(ChromeIntent::ShowPerson(person)) => {
                    self.people.borrow().show_person(&person);
                }
                // The floating clock: what the person pressed there is
                // Clocks' own to carry out, or the bar's to put away.
                DesktopAsk::Intent(ChromeIntent::ClockCommand {
                    command,
                    activity_id,
                }) => self.clock.press(&command, activity_id.as_deref()),
                DesktopAsk::Intent(ChromeIntent::HideClock) => self.clock.hide(),
                // A click on an application's tray icon is the
                // application's own to answer.
                DesktopAsk::Intent(ChromeIntent::AppTray { key, request }) => {
                    self.tray_request(&key, request);
                }
                DesktopAsk::Launchpad(action) => self.launchpad(action),
                DesktopAsk::Shortcut(action) => self.shortcut(action),
                DesktopAsk::HostAction(action) => self.host_action(&action),
                DesktopAsk::Music(action) => self.music(action),
                DesktopAsk::LeftWorkspace(name) => workspaces::left_workspace(&self.notices, &name),
                other => tracing::debug!(?other, "desktop ask the session has no part in yet"),
            }
        }
        // What the person chose where only a host performs it: search and a
        // banner hand their host action back here, as the ask on the
        // person's own avatar does.
        for choice in self.handle.take_host_choices() {
            self.host_action(&choice);
        }
        mount::quick_settings::answer(self);
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

    /// Hand a click on the tray icon at `key` to the application that owns
    /// it, as the click reached the bar. An item that went away between the
    /// click and this ask, or a request the application refuses, is said so
    /// in the log rather than passed over in silence: the person saw their
    /// click do nothing and this is why.
    fn tray_request(&self, key: &str, request: tilo_desktop::chrome::AppTrayRequest) {
        use tilo_desktop::chrome::AppTrayRequest;
        let Some(tray) = &self.tray else {
            tracing::warn!(key, "no tray is served: the click reaches no application");
            return;
        };
        let request = match request {
            AppTrayRequest::Activate { x, y } => tilo_tray_host::TrayRequest::Activate { x, y },
            AppTrayRequest::SecondaryActivate { x, y } => {
                tilo_tray_host::TrayRequest::SecondaryActivate { x, y }
            }
            AppTrayRequest::ContextMenu { x, y } => {
                tilo_tray_host::TrayRequest::ContextMenu { x, y }
            }
        };
        let named = key.to_owned();
        let refused = tray.request_or_else(key, request, move |error| {
            tracing::warn!(key = %named, %error, "a tray click did not reach its application");
        });
        if let Err(error) = refused {
            tracing::warn!(key, %error, "a tray click did not reach its application");
        }
    }

    /// Carry out the host action `id` the person chose: what only the
    /// session can do about workspaces, and, for the offers that are not
    /// about workspaces, what answering a question about the person
    /// themselves does ([`identity::answered`]).
    fn host_action(&self, id: &str) {
        match id {
            NEW_WORKSPACE => self.new_workspace(),
            WORKSPACE_SETTINGS => self.open_workspace_settings(),
            JOIN_WORKSPACE => workspaces::open_directory(
                self.directory.as_ref(),
                &self.handle,
                &self.directory_store,
            ),
            LEAVE_WORKSPACE => self.leave_workspace(),
            choice => identity::answered(choice),
        }
    }

    /// Open what the person chose on a scheduled notification, in the
    /// workspace that scheduled it: the app that takes its scheme.
    fn open_notifications(&self) {
        for (room, uri) in self.notifications.borrow().take_opened() {
            self.search.perform(
                &room,
                tilo_desktop_search::Target::Open {
                    uri,
                    fallback: None,
                },
                &mut |own| self.doors.invoke(&own.address, &own.arguments).map(drop),
            );
        }
    }

    /// Feed the panel's Clipboard section when it changed: every copy the
    /// person can paste again, newest first.
    fn feed_clipboard(&self) {
        let Some(device) = &self.device else {
            return;
        };
        let Some(view) = self.clipboard.changed_view(ui::time::unix_now_ms()) else {
            return;
        };
        device.update(|state| state.clipboard = view);
    }

    /// The workspace in front, as its room: where a chosen notification's
    /// app opens.
    fn active_room(&self) -> String {
        self.handle.read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.room().to_string())
                .unwrap_or_default()
        })
    }

    /// Feed the panel's notification centre when it changed: the retained
    /// notifications, why fresh banners are held and whether a Clear all can
    /// be undone.
    fn feed_notifications(&self) {
        let Some(device) = &self.device else {
            return;
        };
        let view = self.notifications.borrow().view();
        if self.notice_view.borrow().as_ref() == Some(&view) {
            return;
        }
        *self.notice_view.borrow_mut() = Some(view.clone());
        device.update(|state| state.notifications = view);
    }

    /// Show workspace settings for the workspace in front.
    fn open_workspace_settings(&self) {
        workspace_settings::open(
            self.workspace_settings.as_ref(),
            &self.workspace_settings_feed(),
        );
    }

    /// What workspace settings feeds its panes from.
    fn workspace_settings_feed(&self) -> workspace_settings::Feeding<'_> {
        workspace_settings::Feeding::new(
            &self.handle,
            &self.person,
            self.app_lists(),
            &self.ws_settings_fed,
            &self.workspace_emoji,
            self.file_picker.as_ref(),
            self.personal.as_ref(),
        )
    }

    /// This machine's applications, as the session's own lists read them.
    fn app_lists(&self) -> std::sync::Arc<AppLists> {
        self.search.app_lists()
    }

    /// Make a new workspace through the desk's own published action, so the
    /// rules and the naming are the desk's.
    fn new_workspace(&self) {
        self.ask_and_report("new-workspace", &serde_json::Map::new());
    }

    /// Leave the workspace the person is in through the desk's own published
    /// action, which takes it off this device and says it went.
    fn leave_workspace(&self) {
        self.ask_and_report("leave-workspace", &serde_json::Map::new());
    }

    /// Carry out the desk's action `action` as the person's ask, with `args`
    /// naming what the action works on.
    fn ask_of_desk(
        &self,
        action: &str,
        args: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), String> {
        let address = ActionAddress::new(CHROME_RESOURCE, action);
        self.doors
            .invoke(&address, args)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// Ask the desk for `action`, and say so when it refuses.
    fn ask_and_report(&self, action: &str, args: &serde_json::Map<String, serde_json::Value>) {
        if let Err(error) = self.ask_of_desk(action, args) {
            self.handle.acknowledge(error);
        }
    }

    /// Keep this device's desk as it stands: its workspaces, the windows in
    /// them and the strips and displays they are laid out on, so the next
    /// session starts here.
    fn keep_desk_now(&self) {
        let tree = self.handle.read(Clone::clone);
        let Some(value) = desk_layout::value(&tree) else {
            return;
        };
        self.record_desk(&value);
        let Some(state) = &self.state else {
            return;
        };
        let Some(text) = serde_json::to_string(&value).ok() else {
            return;
        };
        let unchanged = self
            .kept
            .borrow()
            .as_ref()
            .is_some_and(|(kept, _)| *kept == text);
        if !unchanged && let Err(error) = desk_layout::save_at(state, &text) {
            // The next pass tries again: the desk is written when it can
            // be, not dropped because a write failed.
            tracing::debug!(%error, "the desk is not kept");
            return;
        }
        *self.kept.borrow_mut() = Some((text, Instant::now()));
    }

    /// [`Self::keep_desk_now`], once in a while: what the workspace's
    /// history records still counts every change, since a drag or a rename
    /// is one change of the workspace however many frames it takes.
    fn keep_desk(&self) {
        let tree = self.handle.read(Clone::clone);
        let Some(value) = desk_layout::value(&tree) else {
            return;
        };
        if !self.record_desk(&value) {
            return;
        }
        let now = Instant::now();
        if self
            .kept
            .borrow()
            .as_ref()
            .is_some_and(|(_, at)| now.duration_since(*at) < KEEP_DESK_EVERY)
        {
            return;
        }
        self.keep_desk_now();
    }

    /// Record the desk as the workspace's history has it, when it changed
    /// since the last record: whether it did. The same desk is one change,
    /// however often it is looked at.
    fn record_desk(&self, value: &serde_json::Value) -> bool {
        if self.recorded.borrow().as_ref() == Some(value) {
            return false;
        }
        self.activity.record_desk(value, activity::now_seconds());
        *self.recorded.borrow_mut() = Some(value.clone());
        true
    }

    /// Take up what the workspace's history wrote back and what the person
    /// opened from it, and tell the apps following it what moved.
    fn serve_activity(&self) {
        if let Some(tree) = self.activity.take_restored() {
            self.handle.replace_tree(tree);
        }
        for (room, target) in self.activity.take_openings() {
            self.open_from_history(&room, &target);
        }
        self.activity.tell_followers();
        if self.activity.take_woken() {
            // A window asked the activity model something: the desk draws a
            // frame, which serves it.
            self.desktop.with_widgets(WidgetHost::relayout);
        }
    }

    /// Open what the person chose in the history: the app that holds it,
    /// showing it.
    fn open_from_history(
        &self,
        room: &tilo_desktop_model::RoomId,
        target: &ui::HistoryResourceTarget,
    ) {
        let Some(apps) = &self.launch else {
            return;
        };
        let Some(app) = target.app_id.as_deref() else {
            return;
        };
        let open = if let Some(path) = target.id.strip_prefix("file:") {
            Some(tilo_app_launch::Open::File(std::path::PathBuf::from(path)))
        } else if target.id.contains("://") {
            Some(tilo_app_launch::Open::Uri(target.id.clone()))
        } else {
            None
        };
        let launch = tilo_app_launch::Launch::Package {
            app: app.to_owned(),
            open,
        };
        let room = apps.room_named(&room.to_string());
        if let Err(error) = apps.start(&launch, &room) {
            tracing::debug!(%error, %app, "an app is not opened from the history");
        }
    }

    /// The windows this device left open: ask for the next one once the one
    /// before it came, each into the tab it was in.
    fn follow_restore(&self) {
        self.restore
            .borrow_mut()
            .follow(&self.desktop, self.launch.as_deref());
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
                    PowerEffect::ShutDown if self.hosted => {
                        self.commands.borrow_mut().push(SessionCommand::End);
                        continue;
                    }
                    PowerEffect::SwitchUser | PowerEffect::Suspend | PowerEffect::Restart
                        if self.hosted =>
                    {
                        // The desktop it runs in owns the machine's power and
                        // its session switching; the menu does not offer
                        // these here, and a published action that asks anyway
                        // stops at this session.
                        tracing::info!(
                            ?effect,
                            "a session in a window owns no power: nothing was done"
                        );
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

        workspaces::follow_directory(
            self.directory.as_ref(),
            &self.handle,
            &self.directory_store,
            &|joined| self.add_joined_workspace(joined),
        );
        workspace_settings::follow(
            self.workspace_settings.as_ref(),
            &self.workspace_settings_feed(),
            &self.intents,
            &self.invocations,
        );
        workspace_settings::refresh(
            self.workspace_settings.as_ref(),
            &self.workspace_settings_feed(),
        );
        // A program of this login asking to work as an agent is a question
        // of the person's: their answer admits it, and the client is served
        // from the next pass on.
        if let Some(door) = self.agent_door.borrow_mut().as_mut() {
            door.follow(&self.handle, &self.person);
        }
        self.asker.follow();
        self.shortcut_guide.follow();
        self.app_sheets.follow();
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
            ShortcutAction::NextInputSource => self
                .commands
                .borrow_mut()
                .push(SessionCommand::NextInputSource),
            ShortcutAction::DictationToggle
            | ShortcutAction::DictationFinish
            | ShortcutAction::DictationCancel => self.dictation.shortcut(action),
            ShortcutAction::ShortcutHelp => self.shortcut_guide.toggle(),
            other => tracing::debug!(?other, "shortcut the session has no part in yet"),
        }
    }
}

/// The notification banners, mounted on a layer of their own over the
/// first output; the desk then shows only the centre of `notices`.
fn mount_banners(
    compositor: &mut Compositor,
    doors: &mut Doors,
    notices: &tilo_desktop_surface::DesktopNotifications,
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
            notices.show_banners_elsewhere();
        }
        Err(error) => tracing::warn!(%error, "notification banners not mounted"),
    }
    banners
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
    /// The login screens' comforts, kept equal to the person's own.
    comforts: Option<mount::comforts::Mirror>,
    /// Checking the passwords the lock screen was given.
    unlocking: Unlocking,
    /// How long the lock has had no lock screen, and how many times the
    /// session put it up again: a lock is lifted rather than left with
    /// nothing that can unlock it.
    lock_watch: LockWatch,
    /// When the person's other devices were last read for their sessions:
    /// their fabric says what it says for a while, and following it more
    /// often would change nothing.
    owner_session_poll: Option<Instant>,
    /// The screenshots asked for.
    captures: Option<capture::Captures>,
    /// The capture studio the session shows, and answers the asks of.
    capture_studio: Option<capture_studio::CaptureStudio>,
    /// The windows opening and closing.
    windows: window_motion::WindowMotions,
    /// Whether the night light is on, and whether a change is kept in the
    /// person's settings.
    night_light: (bool, bool),
    /// The portal's link to the loop, while the portal serves.
    portal: Option<portal_link::SessionLink>,
    /// The apps this session started, told to the portal: which of them run
    /// in one of the launcher's sandboxes.
    launches: std::sync::Arc<Launches>,
    /// The plane the apps it runs read and invoke its services on, while it
    /// serves one.
    action_plane: Option<Plane>,
    /// The shortcuts the person saved, started by their triggers.
    automation: automation::Automation,
    /// The runtime the shortcuts' runs execute on, which the apps' broker
    /// starts them on. Kept for the session's life: dropping it stops the
    /// record feed and the runs still executing on it.
    _automation_runtime: tokio::runtime::Runtime,
    /// The desktop services the portal and the notifications daemon run
    /// in, fed what the session knows of itself.
    desktop: Option<Rc<desktop_services::DesktopServices>>,
    /// The apps the portal was last told run, and how.
    running: Vec<(String, u32)>,
    /// Whether the screens were locked when the portal was last told.
    screen_locked: bool,
    /// The app a process runs, for the windows it shows.
    pids: std::collections::BTreeMap<u32, String>,
    /// The serial of the icon the desk was last handed each window's own
    /// artwork at, and which windows it was told of at all, so a window that
    /// sets another icon is handed the new one and a window that closed lets
    /// go of its own.
    window_artwork: std::collections::BTreeMap<u64, u64>,
    /// The watch that keeps the notification policy current; dropping it
    /// stops the news.
    _settings_watch: Option<settings_store::watch::SettingsWatch>,
    /// The surface apps the session started, and those still running.
    surfaces: surfaces::Surfaces,
    /// The surface apps the session starts itself, once its outputs are
    /// there: the bar and what else the desktop runs as a process of its
    /// own.
    surface_apps: Vec<surfaces::Surface>,
    /// The displays as the person arranged them, kept in step with the ones
    /// attached.
    displays: display_settings::Displays,
    /// The question about a display change, and the answers it takes.
    confirmation: display_confirm::Confirmation,
    /// The display sheet: where the desktop appears, and the wireless
    /// displays it streams to.
    display_sheet: display_sheet::DisplaySheet,
    /// The frames a live projection is sent.
    projection_video: projection_video::ProjectionVideo,
    /// Which way up the device is held, and the lock that pins the panel.
    rotation: rotation::Rotation,
    /// The keyboard input sources, and the one that types.
    sources: config::Sources,
    /// The machine's power: the idle policy, the service over logind and
    /// what holds it awake.
    machine: mount::power::PowerRun,
    /// The shell's bar, when its package started.
    shell: Option<shell::Shell>,
    /// The shell-ui socket the apps ask about their own tabs on, while it
    /// serves.
    tabs: Option<mount::app_tabs::AppTabs>,
    /// The controllers in the room, and how they drive the desktop; `None`
    /// when this session reads none.
    controllers: Option<controllers::Controllers>,
    /// The controllers' descriptors, polled by the loop so their input ends
    /// its wait, by the key the controller source gave each.
    controller_wakes: std::collections::BTreeMap<u64, tilo_compositor::InputWatch>,
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

/// The lock screen's absence while the session is locked, and what the
/// session did about it: a screen that does not come back must not leave the
/// machine locked with nothing able to unlock it.
#[derive(Default)]
struct LockWatch {
    /// When the lock screen was found gone, if it is.
    gone_since: Option<Instant>,
    /// How many times the session has put the lock screen up again for the
    /// lock in force.
    raised: u32,
}

impl Session {
    /// A session on the compositor `start` makes with the desktop's window
    /// policy.
    ///
    /// # Errors
    /// The compositor did not start.
    #[expect(
        clippy::too_many_lines,
        reason = "the session is wired in one pass, where every part sees the others"
    )]
    pub fn new(
        mut options: Options,
        start: impl FnOnce(Box<dyn WindowPolicy>) -> Result<Compositor, Error>,
    ) -> Result<Self, Error> {
        // The words a notice or a query resolves later, in the person's
        // language, are read now: the loop that shows them reads no file.
        i18n::ensure();
        let sources = config::Sources::from_config(&options.config);
        let mut doors = Doors::default();
        // The VPN profiles' kill switch runs for the session's lifetime: a
        // profile set to connect and down blocks traffic while its switch
        // is on, and what blocks is published for Settings.
        if let Some(kill_switch) = options.kill_switch.take() {
            kill_switch.start();
        }
        // What the desk and the chrome ask of the session, from their own
        // widget trees.
        let notices = tilo_desktop_surface::DesktopNotifications::default();
        // What the bar reads of the notifications, and the policy arrivals
        // are judged by: the person may change it at any time.
        let notice_signals = Rc::new(notifications::NoticeSignals::default());
        let notification_policy = std::sync::Arc::new(std::sync::Mutex::new(
            settings_store::notification_policy::NotificationPolicy::load(),
        ));
        let watched = std::sync::Arc::clone(&notification_policy);
        let woken = options.wake.handle();
        // The pictures the desk shows are this device's, kept beside its
        // desk: a change of the person's reaches them the same way.
        let wallpapers = std::sync::Arc::new(wallpapers::Wallpapers::new(
            options.state.clone(),
            std::sync::Arc::clone(&woken),
        ));
        let watched_wallpapers = std::sync::Arc::clone(&wallpapers);
        // The desk widgets' choices, kept current by the same watch.
        let widget_prefs = desk::prefs();
        let watched_widgets = std::sync::Arc::clone(&widget_prefs);
        // Whether what the person plays here may go on playing on their other
        // devices, read from the same settings document on the same watch.
        let media = fabric::MediaContinuity::default();
        media.read(&settings_store::load());
        let watched_media = media.clone();
        // Artwork the session resolves off the loop wakes it when it is
        // ready, so the frame that painted the letter repaints with it; the
        // apps installed here are the lists' to resolve, on their worker.
        app_icons::set_wake(options.wake.handle());
        app_icons::set_lists(options.search.lists.clone());
        let settings_watch = settings_store::watch::watch_settings(move || {
            let document = settings_store::load();
            *watched.lock_recover() =
                settings_store::notification_policy::NotificationPolicy::from_document(&document);
            watched_wallpapers.follow(&document);
            desk::follow_prefs(&watched_widgets, &document);
            watched_media.read(&document);
            woken();
        })
        .map_err(|error| tracing::debug!(%error, "settings are not watched"))
        .ok();
        let presentation = (options.presentation)();
        let overview = mount::overview::Overview::new(
            presentation.theme.motion(),
            presentation.theme.reduce_motion,
        );
        // The desk's open card, which the keys fold on Escape and the
        // overview folds as it comes over the desk: the desk is under the
        // windows and hears neither itself.
        let fold = tilo_desktop::desk_widgets::DeskFold::default();
        // What plays, which the desk's music widget shows: the session keeps
        // it current from the player Quick Settings already reads, so the
        // widget is a view of that one reading.
        let music = tilo_desktop::desk_widgets::DeskMusic::default();
        // The workspace's activity: the desk's own log, and the records its
        // apps keep in the store. The desk records into it, the model reads
        // it, and the apps this session runs reach the same model.
        let activity = activity::Activity::new(
            options.state.as_deref(),
            options.person.clone(),
            options.search.records.as_ref().map(records::Records::root),
            options.search.lists.clone(),
        );
        let (desktop, mut policy, asks) = desk::desk(
            &options,
            &activity,
            notices.clone(),
            std::sync::Arc::clone(&wallpapers),
            widget_prefs,
            fold.clone(),
            overview.open_flag(),
            music.clone(),
        );
        desktop.set_gestures(options.config.gestures);
        desktop.set_theme(presentation.theme);
        let handle = desktop.with_policy(|policy| policy.handle().clone());
        // The desk as this device left it: its workspaces, the windows they
        // held, and the apps those windows open again.
        let mut restore = desk_restore::Restore::default();
        let mut keeps_desk = true;
        if let Some(state) = options.state.as_deref() {
            match desk_layout::load_at(state) {
                desk_layout::Kept::Desk(kept) => {
                    restore = desk_restore::Restore::of(&kept);
                    handle.replace_tree(*kept);
                }
                // The person's desk is on disk and this session did not open
                // it: they are looking at a desk that is not theirs, and have
                // to be told where their own still is.
                desk_layout::Kept::Refused(refusal) => {
                    keeps_desk = false;
                    notices::tell_desk_not_opened(&notices, &refusal);
                }
                desk_layout::Kept::None => {}
            }
        }
        workspaces::publish_host_actions(&handle);
        // What the person's settings say the desk shows: a device that kept
        // no picture yet takes the choice, and otherwise the workspaces keep
        // the pictures this device kept for them until the setting changes.
        wallpapers.set_active(handle.read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.id.0.to_string())
        }));
        wallpapers.start(&settings_store::load());
        // The apps the person opens read their room from this desktop and
        // leave their notice in it.
        if let Some(launch) = &options.launch {
            launch.attach(desktop.clone());
        }
        // One process per app per room: its later windows open in the
        // process the session started, with connections of their own. A
        // window mapping settles what its app's door serves, so an app built
        // before the door keeps a process per window from then on.
        let window_doors = WindowDoors::new(mount::app_tabs::ServiceLinks::new(
            std::sync::Arc::clone(&options.launches),
        ));
        let settling = Rc::clone(&window_doors);
        policy.set_window_mapped(Rc::new(move |pid| settling.window_mapped(pid)));
        if let Some(launch) = &options.launch {
            launch.lend_doors(Rc::clone(&window_doors));
        }

        // The programs the person downloaded: the registry takes the
        // windows the desk gives a tab as they appear, so one that names no
        // application directory is offered the first time it shows itself.
        downloaded::install(&mut options, &mut policy);
        let mut compositor = start(Box::new(policy))?;
        // A session in a window of another desktop runs no idle timers: that
        // desktop owns idle, blanking, locking and suspend.
        compositor.set_hosted(options.hosted);
        // What the services take in wakes the loop instead of waiting for
        // its next pass.
        options.wake.bind(compositor.waker());
        compositor.set_input_config(options.config.input.clone());
        compositor.set_keyboard_layout(&options.config.keyboard);
        compositor.set_idle_config(options.config.idle);
        a11y_settings::apply(&mut compositor, &options.config.a11y)?;
        compositor.set_backlight(Box::new(display_settings::PanelBacklight::find()));
        let machine = mount::power::install(&mut compositor, &options);
        compositor.add_host(Box::new(desktop.clone()));
        doors.add(Rc::new(RefCell::new(desktop.clone())));

        // The chrome above the windows: its own tree, beside the desk's.
        let intents = Rc::new(RefCell::new(Vec::new()));
        let engine = open_engine(&options, &handle);
        let sync = start_collaboration(&options, media, engine.clone());
        // The person's own workspaces, which their other devices hold with
        // them: kept beside the desk, and opened when this computer knows
        // whose they are.
        let personal = workspaces::personal_for(options.state.as_deref());
        // The fabric the session acts on: one handed to it — a stand-in in a
        // test — or its own, when the person certified this device. A
        // session that takes part as nobody keeps none.
        let own_fabric = match &options.fabric {
            Some(_) => None,
            None => sync.as_ref().and_then(|sync| sync.fabric()),
        };
        let fabric: Option<tilo_device_fabric::link::SharedLink> =
            match (&options.fabric, own_fabric) {
                (Some(shared), _) => Some(Arc::clone(shared)),
                (None, Some(own)) => Some(own),
                (None, None) => None,
            };
        // The device's readings, which the bar's status cluster shows as the
        // battery, network and volume glyphs; the quick settings panel fills
        // them once it is mounted.
        let readings = Rc::new(RefCell::new(None));
        // Every app shows its live activity on this one board, which the
        // bar's clock reads and the controls pressed on the clock go back
        // through.
        let live_activities = std::sync::Arc::new(runtime_collab::LiveActivities::default());
        // A timer starting in the background must reach the bar's clock on
        // the next frame, not the next minute.
        live_activities.set_host_wake(options.wake.handle());
        let clock = Rc::new(chrome::Clock::new(std::sync::Arc::clone(&live_activities)));
        // An application's tray item registering, changing or going away is
        // read off the loop: it must reach the bar on the next frame.
        let tray = options
            .desktop
            .as_ref()
            .and_then(|desktop| desktop.tray().cloned());
        if let Some(tray) = &tray {
            tray.set_host_wake(options.wake.handle());
        }
        // The displays a fullscreen window owns, which the loop takes from
        // the compositor every pass: the bar there steps aside for it.
        let fullscreen: Rc<RefCell<Vec<String>>> = Rc::default();
        // The emoji picker is the chrome's; the session holds its picks,
        // writes them into the focused field and keeps its recents.
        let emoji = emoji::Emoji::mount();
        let chrome = Rc::new(RefCell::new(chrome::host(
            &handle,
            &options,
            Rc::clone(&intents),
            Rc::clone(&asks),
            chrome::BarFeeds {
                signals: Rc::clone(&notice_signals),
                readings: Rc::clone(&readings),
                clock: Rc::clone(&clock),
                notices: notices.clone(),
                tray: tray.clone(),
                fullscreen: Rc::clone(&fullscreen),
            },
            sync.clone(),
            &overview,
            &emoji.link(),
        )));
        compositor.add_host(Box::new(Held(Rc::clone(&chrome))));
        doors.add(Rc::clone(&chrome) as tilo_surface_host::Shared);

        // The desk as the session publishes it, and the shell's bar over the
        // chrome, which reads it through the door.
        let desk = || desk_resource::Desk {
            handle: handle.clone(),
            person: options.person.clone(),
            signals: Rc::clone(&notice_signals),
            clock: Rc::clone(&clock),
            status_panel: options.quick_settings.is_some(),
            sync: sync.clone(),
            tray: tray.clone(),
        };
        let desk_service = desk();
        let shell = options
            .shell_bar
            .then(|| {
                shell::Shell::mount(&mut compositor, &mut doors, desk(), options.scale)
                    .map_err(|why| tracing::error!(why, "the shell's bar did not start"))
                    .ok()
            })
            .flatten();
        // Who is here, over the windows: the roster the bar's faces open and
        // where everyone else's pointer is.
        let people = people::mount(&mut compositor, &mut doors, &options, &handle, sync.clone());

        let banners = mount_banners(&mut compositor, &mut doors, &notices, &options);

        let (device, quick_settings) =
            mount_device(&mut compositor, &mut doors, &mut options, &readings);

        // The sheets, over the desktop and its panels.
        let mount::sheets::Sheets {
            power,
            workspace_settings,
            portal,
            file_picker,
            directory,
            lock,
            lock_mount,
            display_confirm,
            displays: display_seat,
            shortcut_guide: guide_seat,
            add_app,
            uninstall,
        } = mount::sheets::mount_sheets(&mut compositor, &mut doors, &options);

        // The capture studio, over everything it captures.
        let capture_studio =
            capture_studio::mount(&mut compositor, &mut doors, &options, fabric.as_ref());

        // One table for the keys the session answers and the guide that
        // explains them: a chord that works is a chord the guide shows.
        let shortcuts = Rc::new(RefCell::new(std::mem::take(&mut options.config.shortcuts)));
        // The desktop's own chords win over every app's: the portal's
        // bindings step aside for the table the session answers first.
        let portal_shortcuts = options
            .desktop
            .as_ref()
            .and_then(|desktop| desktop.portal())
            .map(|portal| portal.shortcuts().clone());
        if let Some(portal) = &portal_shortcuts {
            portal.set_shortcut_table(shortcuts.borrow().clone());
        }
        // The guide's keys while it is up: the key surface holds back what
        // would act behind the guide and hands it the chord it waits for.
        let guide_keys = GuideKeys::default();
        // The controller the person last used: the guide leads with its
        // buttons, and a key of their own leads with the keyboard again.
        let controller: Rc<Cell<Option<ui::ControllerFamily>>> = Rc::new(Cell::new(None));
        let controller_keys: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        let shortcut_guide = shortcut_guide::ShortcutGuide::new(
            guide_seat,
            Rc::clone(&shortcuts),
            guide_keys.clone(),
            portal_shortcuts.clone(),
            Rc::clone(&controller),
        );
        let (commands, invocations) = mount_controls(
            &mut compositor,
            &mut doors,
            shortcuts,
            portal_shortcuts,
            guide_keys,
            KeyContext {
                controller: Rc::clone(&controller),
                controller_keys: Rc::clone(&controller_keys),
                displays: display_seat
                    .as_ref()
                    .map(|seat| tilo_sheets::Seat::presentation(seat).clone()),
                fold,
            },
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

        // What the broker admits a connection by, and what an app started
        // from here on is written into.
        if let Some(apps) = &options.launch {
            apps.lend_launches(std::sync::Arc::clone(&options.launches));
        }

        // The shortcuts the person saved and the triggers that start them
        // without anyone invoking them: armed on the desk this device shows,
        // and started on the broker below. A run executes here, on a runtime
        // of its own, and what a person saves through the action door lands
        // on the desk on the loop's next pass.
        let automation_runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("tilo-automation")
            .enable_all()
            .build()?;
        let (shortcut_library, saved_shortcuts) =
            plane::ShortcutLibrary::new(automation_runtime.handle().clone());
        // The runs people ask of this session's agents: the session keeps
        // its own, or plays the run channel a test handed it. Its broker
        // delivers them, and it is one of the session's services.
        let agent_runs = options.agent_runs.take().unwrap_or_else(|| {
            std::sync::Arc::new(tilo_agent_runs::AgentRuns::new(Some(options.wake.handle())))
        });
        let action_plane = start_action_plane(
            &options,
            &live_activities,
            &handle,
            std::sync::Arc::new(shortcut_library),
            &activity,
            std::sync::Arc::clone(&agent_runs),
        )?;
        // The agent door: a program of this login asks here to work in a
        // workspace as an agent, and the person answers on their sheets. It
        // binds first, and is served from the broker the plane starts, which
        // already decides against the desk the loop shows.
        let questions = options.questions.clone().or_else(|| {
            options
                .desktop
                .as_ref()
                .map(|desktop| desktop.questions().clone())
        });
        let door = options.agent_door.as_deref().and_then(|path| {
            match agent_door::mount(
                path,
                questions.clone(),
                std::sync::Arc::clone(&options.launches),
                options.wake.handle(),
                options.state.as_deref(),
            ) {
                Ok(mounted) => Some(mounted),
                Err(error) => {
                    tracing::warn!(
                        %error,
                        path = %path.display(),
                        "the agent door does not open"
                    );
                    None
                }
            }
        });
        let agent_door = door.map(|door| {
            door.serve(
                action_plane.as_ref().and_then(Plane::agent_broker),
                action_plane
                    .as_ref()
                    .map(|plane| std::sync::Arc::clone(&plane.desk)),
            )
        });
        let automation = automation::Automation::new(
            options.state.as_deref(),
            options.wake.handle(),
            handle.clone(),
            options.person.clone(),
            saved_shortcuts,
            // Which of the person's devices this one is, and which of them
            // are online: each schedule runs on one of them.
            sync.as_ref().and_then(|sync| sync.fabric()),
        );

        // The shell-ui socket this session serves: what an app asks about
        // its own tabs arrives there and is answered on this loop's pass.
        let tabs = mount::app_tabs::AppTabs::start(
            std::sync::Arc::clone(&options.launches),
            options.wake.handle(),
        )
        .map_err(|error| tracing::warn!(%error, "the shell-ui service does not run"))
        .ok();
        // What the surface apps this session starts are of it: the launch
        // the broker admits each connection by, and the socket it answers
        // them on.
        let mut surfaces = surfaces::Surfaces::default();
        surfaces.lend(
            std::sync::Arc::clone(&options.launches),
            action_plane.as_ref().map(|plane| plane.path.clone()),
        );
        let mut started = std::mem::take(&mut options.services);
        started.push(Box::new(desk_service));
        let services = mount_services(&mut doors, started);
        services
            .borrow_mut()
            .add(Box::new(tilo_agent_runs::Served::new(
                std::sync::Arc::clone(&agent_runs),
            )));
        // Dictation joins the services: its recognizer, microphone and
        // consent are only the session's, and the sheet asks about them.
        let dictation =
            dictation::Dictation::mount(&mut compositor, &mut doors, &services, &options);
        // The questions about the person's apps answer through the registry
        // of their downloaded programs, which is one of the services.
        let app_sheets =
            app_sheets::AppSheets::new(add_app, uninstall, Rc::clone(&services), notices.clone());

        let search = Rc::new(SessionSearch::new(options.search, options.apps));
        let asker = questions::Asker::new(
            questions,
            portal.clone(),
            file_picker.clone(),
            Rc::clone(&services),
        );
        let workspace_emoji =
            workspace_settings::EmojiTab::new(cli::data_home(), options.wake.handle());
        let shared = Shared {
            handle,
            desktop,
            activity,
            person: options.person.clone(),
            search,
            launch: options.launch,
            restore: RefCell::new(restore),
            drives: options.drives,
            asks,
            intents,
            quick_settings,
            doors,
            invocations,
            commands,
            readings,
            services,
            agent_runs,
            hosted: options.hosted,
            device,
            fabric,
            devices: mount::devices::Devices::default(),
            waking: mount::wake::Wake::new(&options.wake_seams),
            music,
            power,
            workspace_settings,
            ws_settings_fed: Cell::new(None),
            workspace_emoji,
            directory,
            directory_store: directory::Store::default(),
            portal,
            file_picker,
            lock,
            owner_session: owner_session::SessionFollow::default(),
            lock_mount,
            display_confirm,
            notices: notices.clone(),
            clock,
            tray,
            kept: RefCell::new(None),
            recorded: RefCell::new(None),
            // A desk this session did not open is never written over: it
            // keeps nothing, so it cannot be.
            state: keeps_desk.then(|| options.state.clone()).flatten(),
            wallpapers,
            lock_power: options.lock_power,
            lock_layouts: options.config.keyboard_layouts.clone(),
            lock_layout: Cell::new(0),
            session_actions: options.session_actions,
            observed: RefCell::new(None),
            places_seen: RefCell::default(),
            notifications: RefCell::new(notifications::SessionNotifications::new(
                options
                    .desktop
                    .as_ref()
                    .map(|desktop| desktop.notifications().clone()),
                notices,
                banners.clone(),
                notification_policy,
                notice_signals,
                options.power.clone(),
            )),
            asker,
            agent_door: RefCell::new(agent_door),
            notice_view: RefCell::new(None),
            sync,
            engine,
            personal,
            clipboard: mount::clipboard::Clipboard::new(),
            settings: mount::settings_sync::SettingsSync::new(),
            overview,
            dictation,
            emoji,
            shortcut_guide,
            chrome,
            people,
            fullscreen,
            controller,
            controller_keys,
            app_sheets,
        };
        shared.start_search();
        // What the portal brings comes from any thread, and wakes the loop
        // instead of waiting for its next turn.
        if let Some(portal) = &options.portal {
            compositor.set_input_observer(Some(portal.input_observer()));
        }
        let loop_wake = compositor.waker();
        // Signing in does not change the login screens; what the person
        // changes here afterwards does, where they let it follow.
        let comforts = (!options.hosted).then(|| {
            let wake = loop_wake.clone();
            mount::comforts::Mirror::mount(std::path::PathBuf::from(tilo_auth::SOCKET), move || {
                wake.wake_by_ref();
            })
        });
        let surface_apps = std::mem::take(&mut options.surfaces);
        Ok(Self {
            compositor,
            shared,
            banners,
            input: options.config.input,
            setup,
            comforts,
            unlocking: Unlocking {
                user: options.user,
                verifier: options.verifier,
                verdicts: std::sync::mpsc::channel(),
                wake: loop_wake,
            },
            lock_watch: LockWatch::default(),
            owner_session_poll: None,
            captures: options.captures,
            capture_studio,
            windows: window_motion::WindowMotions::new((options.presentation)().theme.motion()),
            night_light: (options.config.night_light, options.config.kept),
            portal: options.portal,
            launches: std::sync::Arc::clone(&options.launches),
            action_plane,
            automation,
            _automation_runtime: automation_runtime,
            screen_locked: false,
            desktop: options.desktop,
            running: Vec::new(),
            pids: std::collections::BTreeMap::new(),
            window_artwork: std::collections::BTreeMap::new(),
            _settings_watch: settings_watch,
            surfaces,
            surface_apps,
            displays: display_settings::Displays::default(),
            confirmation: display_confirm::Confirmation::default(),
            display_sheet: display_sheet::DisplaySheet::new(
                display_seat,
                options.projector.clone(),
                &options.wake.handle(),
            ),
            projection_video: projection_video::ProjectionVideo::new(),
            rotation: rotation::Rotation::mount(options.accelerometer),
            sources,
            shell,
            ended: false,
            magnifier: (
                settings_store::magnifier::OFF,
                settings_store::magnifier::DEFAULT_ZOOM,
            ),
            machine,
            tabs,
            controllers: options
                .controllers
                .take()
                .map(controllers::Controllers::mount),
            controller_wakes: std::collections::BTreeMap::new(),
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
        session.warm()?;
        Ok(session)
    }

    /// A session on a bare TTY.
    ///
    /// # Errors
    /// The compositor did not start.
    pub fn drm(options: Options, config: DisplayConfig) -> Result<Self, Error> {
        let mut session = Self::new(options, |policy| Compositor::drm(policy, config))?;
        session.warm()?;
        Ok(session)
    }

    /// A session in a window of the desktop it is started from, for
    /// development (see [`tilo_compositor::backend::nested`]).
    ///
    /// # Errors
    /// The compositor or its window did not start.
    pub fn nested(options: Options, config: &NestedConfig) -> Result<Self, Error> {
        let mut session = Self::new(options, |policy| Compositor::nested(policy, config))?;
        session.warm()?;
        Ok(session)
    }

    /// Warm the displays when the person left the night light on, and start
    /// the surface apps: their windows wait for the outputs the session has
    /// now added.
    fn warm(&mut self) -> Result<(), Error> {
        if self.night_light.0 {
            self.compositor.set_night_light(Some(NIGHT_LIGHT_WARMTH))?;
        }
        self.follow_displays()?;
        self.follow_fullscreen();
        for surface in &self.surface_apps {
            let room = self.shared.active_room();
            self.surfaces
                .start_or_log(&mut self.compositor, surface, &room);
        }
        Ok(())
    }

    /// Bring the displays in line with the person's arrangement and the way up
    /// the device is held, and advance a change waiting for their answer.
    fn follow_displays(&mut self) -> Result<(), Error> {
        let night_light = self.night_light.0.then_some(NIGHT_LIGHT_WARMTH);
        let turned = self.rotation.follow(&self.compositor.outputs());
        self.displays
            .follow(&mut self.compositor, night_light, turned)
    }

    /// Take the displays a fullscreen window owns from the compositor, so
    /// the bar on such a display steps aside this pass. The chrome reads
    /// them when it next builds its snapshot, which is what places the
    /// bars.
    fn follow_fullscreen(&self) {
        let monitors = self.compositor.fullscreen_monitors();
        let shell = &self.shared.fullscreen;
        if *shell.borrow() != monitors {
            shell.replace(monitors);
        }
    }

    /// The keyboard input sources, in the person's order.
    #[must_use]
    pub fn input_sources(&self) -> &[String] {
        &self.sources.ids
    }

    /// The input source the keyboard types in now.
    #[must_use]
    pub fn active_input_source(&self) -> &str {
        &self.sources.active
    }

    /// The compositor, to connect clients, route input and read frames.
    pub fn compositor(&mut self) -> &mut Compositor {
        &mut self.compositor
    }

    /// The surface apps the session started, and those still running.
    #[must_use]
    pub const fn surfaces(&self) -> &surfaces::Surfaces {
        &self.surfaces
    }

    /// Start `surface` now, as a client of the running compositor, as the app
    /// it runs for in the workspace in front.
    ///
    /// # Errors
    /// Its socket or process could not be made.
    pub fn start_surface(
        &mut self,
        surface: &surfaces::Surface,
    ) -> Result<(), surfaces::StartError> {
        let room = self.shared.active_room();
        self.surfaces.start(&mut self.compositor, surface, &room)
    }

    /// Stop the surface app `name`, if it runs.
    pub fn stop_surface(&mut self, name: &str) {
        self.surfaces.stop(name);
    }

    /// Ask the surface app `name` to run `action` of `resource`, as one of
    /// the actions it publishes, and name the invocation `id`; `None` when
    /// no such app runs. Its answering carries on in the session's turns
    /// ([`Surfaces::outcome`](surfaces::Surfaces::outcome)).
    pub fn ask_surface(
        &mut self,
        name: &str,
        resource: &str,
        action: &str,
        argument: Option<&str>,
    ) -> Option<u64> {
        self.surfaces.ask(name, resource, action, argument)
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

    /// The desk's notifications: the banners it is showing now and the
    /// centre it keeps behind them.
    #[must_use]
    pub fn notifications(&self) -> tilo_desktop_surface::DesktopNotifications {
        self.shared.notifications.borrow().desktop()
    }

    /// The board the apps show their live activities on, which the bar reads
    /// as its clock.
    #[must_use]
    pub fn live_activities(&self) -> &std::sync::Arc<runtime_collab::LiveActivities> {
        self.shared.clock.board()
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

    /// The windows the overview pictures as the compositor draws them now:
    /// each window's id and the rect its picture rests at, logical
    /// `(x, y, width, height)`, bottom first. Empty while the overview is
    /// away, and before a window on the desk has a picture.
    #[must_use]
    pub fn overview_pictures(&self) -> Vec<(u64, (f64, f64, f64, f64))> {
        self.shared.overview_pictures()
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

    /// The lock screen's seat, while the session has one: whether it is up,
    /// and what it shows.
    #[must_use]
    pub fn lock_screen(&self) -> Option<&LockSeat> {
        self.shared.lock.as_ref()
    }

    /// The add sheet's seat: the program the person is asked about, when
    /// one is; `None` when the sheet did not mount.
    #[must_use]
    pub fn add_app_sheet(&self) -> Option<&tilo_sheets::add_app::AddSeat> {
        self.shared.app_sheets.add()
    }

    /// The uninstall sheet's seat: the app the person is asked about, when
    /// one is; `None` when the sheet did not mount.
    #[must_use]
    pub fn uninstall_sheet(&self) -> Option<&UninstallSeat> {
        self.shared.app_sheets.uninstall()
    }

    /// Ask the person whether to uninstall `app`, by its entry's id in the
    /// downloaded programs' registry or by the name they know it by.
    pub fn ask_uninstall(&self, app: &str) {
        self.shared.app_sheets.ask_uninstall(app);
    }

    /// Workspace settings' seat: what it shows, kept current by whoever
    /// knows the workspace.
    #[must_use]
    pub fn workspace_settings(&self) -> Option<&WorkspaceSettingsSeat> {
        self.shared.workspace_settings.as_ref()
    }

    /// The workspace directory's seat: the workspaces this computer may
    /// join or open, shown when the person asks for them.
    #[must_use]
    pub fn workspace_directory(&self) -> Option<&DirectorySeat> {
        self.shared.directory.as_ref()
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

    /// The shortcut guide's seat: the session puts it up with F1, and it
    /// shows the chords the session answers.
    #[must_use]
    pub fn shortcut_guide(&self) -> Option<&GuideSeat> {
        self.shared.shortcut_guide.seat()
    }

    /// The seat of the question about a display change: what it asks now,
    /// and where it stands.
    #[must_use]
    pub fn display_confirm(&self) -> Option<&ConfirmSeat> {
        self.shared.display_confirm.as_ref()
    }

    /// The display sheet: the layouts the attached displays allow and the
    /// wireless displays the desktop can be streamed to, as it shows them.
    #[must_use]
    pub fn display_sheet(&self) -> Option<&tilo_sheets::displays::DisplaysSeat> {
        self.display_sheet.seat()
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
        // What the history wrote back reaches the desk before anything reads
        // it, and what the person opened from it starts from here.
        self.shared.serve_activity();
        self.follow_collaboration();
        self.follow_automation();
        self.shared.follow_personal();
        self.follow_power()?;
        if let Some(comforts) = &mut self.comforts {
            comforts.observe();
        }
        self.follow_owner_session()?;
        self.follow_lock()?;
        if let Some(captures) = &self.captures {
            captures.serve(&mut self.compositor, self.capture_studio.as_mut());
        }
        if let Some(studio) = &mut self.capture_studio {
            studio.serve(&mut self.compositor);
        }
        if let Some(portal) = &mut self.portal {
            let documents_served = self
                .desktop
                .as_ref()
                .is_some_and(|desktop| desktop.documents_served());
            portal.serve(
                &mut self.compositor,
                &self.shared.handle,
                &self.launches,
                documents_served,
            );
        }
        self.feed_portal();
        self.feed_window_artwork();
        // What the apps copied since the last pass joins the history; a copy
        // chosen in it is pasted once the panel let the keyboard go.
        self.shared.clipboard.follow(&mut self.compositor);
        // The question about a display change, before the sheets are placed:
        // one that just opened stands on this pass's displays.
        if let Some(confirm) = self.shared.display_confirm.as_ref() {
            self.confirmation.follow(
                &mut self.compositor,
                confirm,
                &self.displays,
                &self.shared.notices,
            );
        }
        self.display_sheet.follow(
            &self.compositor.outputs(),
            self.displays.awaiting_confirmation().is_some(),
            &self.shared.invocations,
        );
        let place = self.sheet_place();
        self.place_sheets(place);
        // Each round can ask for another (a key's shortcut the chrome
        // leaves to the session); a few rounds settle any chain.
        for _ in 0..4 {
            self.shared.follow_asks();
            // What was plugged in since, and what the person asked of its
            // notice: an open starts Files like any other launch.
            if let Some(drives) = &self.shared.drives {
                drives.follow(self.shared.launch.as_deref());
            }
            // What the desktop asked to open, and what stopped since.
            if let Some(launch) = &self.shared.launch {
                launch.follow();
            }
            // The windows this device left open, one at a time, each into the
            // tab it was in.
            self.shared.follow_restore();
            // What the apps asked of their own tabs, and of what a capture
            // may show of them, as the desk commands they are.
            if let Some(tabs) = &mut self.tabs {
                tabs.follow(
                    &self.shared.desktop,
                    self.shared.launch.as_deref(),
                    &mut self.compositor,
                );
            }
            self.shared.follow_gestures();
            let ran = self.shared.run_invocations(self.compositor.locked());
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
        self.shared.clipboard.serve_asks(&mut self.compositor);
        // One file of a paste in flight per turn, so a folder of files does
        // not hold up a frame.
        self.shared.clipboard.step_files();
        // What was copied here reaches the person's other devices, and what
        // they copied there joins the clipboard here.
        if let Some(fabric) = &self.shared.fabric {
            let media_enabled = fabric.require_media().is_ok();
            if self.shared.clipboard.sync_fabric(fabric.as_ref()) {
                self.shared.feed_clipboard();
            }
            // What the person's devices sent this one, before this pass's
            // pumps: a route they asked for starts on the same pass it was
            // asked for on.
            if let Some(sync) = &self.shared.sync {
                for envelope in sync.fabric_actions() {
                    self.route_fabric_action(fabric, &envelope);
                }
            }
            // The person's speaker groups, remote audio and the view Settings
            // asks of them move on the same fabric, on the same pass: a
            // device arriving, a group being created or a calibration
            // progressing is what the next snapshot shows.
            tilo_speaker_service::remote_audio::pump(fabric.as_ref(), media_enabled, None);
            tilo_speaker_service::groups::pump(fabric.as_ref(), media_enabled);
            tilo_speaker_service::settings::observe(fabric.as_ref());
            // The person's settings follow them between their devices on the
            // same fabric: what they set here is offered to it, and what
            // they set there reaches the document this session reads.
            self.shared.settings.sync(fabric.as_ref());
            // The person's sleeping devices, and the probes that follow them.
            self.shared.waking.follow(&self.shared);
            // What this pass offered the clipboard, and what the pumps just
            // changed, is this device's next word to its other devices.
            if let Some(sync) = &self.shared.sync {
                sync.announce_fabric();
            }
        }
        Ok(())
    }

    /// Keep the rooms this session takes part in current with the desktop's
    /// workspaces, and hand the desktop who is here.
    fn follow_collaboration(&self) {
        let Some(sync) = &self.shared.sync else {
            return;
        };
        self.shared.handle.read(|tree| sync.follow_desktop(tree));
        sync.follow();
        // What became of the invitations redeemed is the directory's to say,
        // and the desk's to show.
        self.shared.follow_enrollments();
    }

    /// Start the shortcuts whose triggers fired since the last pass, and keep
    /// the apps' broker's copy of the desk level with the desk shown: a run
    /// starting there checks the shortcut against it.
    fn follow_automation(&mut self) {
        if let Some(plane) = &self.action_plane {
            // The broker may take a moment to accept connections after it
            // starts; its runs start from the first pass that finds it.
            if self.automation.needs_broker()
                && let Some(broker) = plane.action_broker()
            {
                self.automation.attach(broker);
            }
            self.automation.settle(Some(&plane.desk));
        } else {
            self.automation.settle(None);
        }
    }

    /// The shortcut runs that finished and no history recorder took yet,
    /// oldest first: what the history names its changes by.
    #[must_use]
    pub fn take_finished_shortcut_runs(&self) -> Vec<FinishedShortcutRun> {
        self.automation.take_finished()
    }

    /// Where every sheet goes: the display the person is working on, so a
    /// question appears in front of what it is about and the power menu
    /// where they are looking. `None` while the desk knows no such display
    /// (one display needs no telling apart), which leaves a sheet on the
    /// first display there is.
    fn sheet_place(&self) -> Option<sheets::Place> {
        let display = self.shared.handle.focused_display()?;
        let monitor = self
            .shared
            .handle
            .read(|tree| tree.outputs.monitor_of(display).map(str::to_owned))?;
        let (x, y, w, h) = self
            .compositor
            .outputs()
            .into_iter()
            .filter(|output| output.enabled)
            .find(|output| output.monitor == monitor)
            .and_then(|output| output.rect)?;
        Some((
            x,
            y,
            u32::try_from(w.max(1)).unwrap_or(1),
            u32::try_from(h.max(1)).unwrap_or(1),
        ))
    }

    /// Tell every sheet which display it covers: the one the person is
    /// working on, whenever the desk names another.
    fn place_sheets(&self, place: Option<sheets::Place>) {
        for presentation in self.sheet_presentations() {
            presentation.place_at(place);
        }
    }

    /// Every mounted sheet's presentation, in the order it draws.
    fn sheet_presentations(&self) -> impl Iterator<Item = &tilo_sheets::Presentation> {
        let shared = &self.shared;
        [
            shared.power.as_ref().map(tilo_sheets::Seat::presentation),
            shared
                .workspace_settings
                .as_ref()
                .map(tilo_sheets::Seat::presentation),
            shared
                .directory
                .as_ref()
                .map(tilo_sheets::Seat::presentation),
            shared.portal.as_ref().map(tilo_sheets::Seat::presentation),
            shared
                .file_picker
                .as_ref()
                .map(tilo_sheets::Seat::presentation),
            shared.shortcut_guide.presentation(),
            shared
                .display_confirm
                .as_ref()
                .map(tilo_sheets::Seat::presentation),
            self.display_sheet.presentation(),
            shared.lock.as_ref().map(tilo_sheets::Seat::presentation),
            self.capture_studio
                .as_ref()
                .map(capture_studio::CaptureStudio::presentation),
            shared.dictation.presentation(),
        ]
        .into_iter()
        .flatten()
    }

    /// Tell the portal what it asks of the session about its apps: which
    /// run and how, which exited, and when the screen locks.
    fn feed_portal(&mut self) {
        let Some(desktop) = &self.desktop else {
            return;
        };
        for (window, _) in self.compositor.windows() {
            if let (Some(pid), Some(app_id)) = (window.pid, window.app_id) {
                desktop.note_running_app(pid, &app_id);
                self.pids.insert(pid, app_id);
            }
        }
        let running = running_apps(&self.compositor);
        if running != self.running {
            desktop.set_running_apps(running.clone());
            self.running = running;
        }
        let locked = self.compositor.locked();
        if locked != self.screen_locked {
            desktop.set_screen_locked(locked);
            self.screen_locked = locked;
        }
        for pid in self.compositor.take_exited_pids() {
            if let Some(app_id) = self.pids.remove(&pid) {
                desktop.app_exited(&app_id);
            }
        }
    }

    /// Hand the desk each window's own icon, which it paints on the window's
    /// tab and in the switcher, and let go of the icons of windows that
    /// closed or set none.
    fn feed_window_artwork(&mut self) {
        let mut shown = std::collections::BTreeMap::new();
        for (window, _) in self.compositor.windows() {
            let Some(icon) = window.icon.as_ref() else {
                continue;
            };
            let serial = icon.serial();
            shown.insert(window.id, serial);
            if self.window_artwork.get(&window.id) == Some(&serial) {
                continue;
            }
            self.shared
                .handle
                .set_window_artwork(window.id, Some(app_icons::window(window.id, icon)));
        }
        for (gone, _) in std::mem::take(&mut self.window_artwork) {
            if !shown.contains_key(&gone) {
                self.shared.handle.set_window_artwork(gone, None);
            }
        }
        self.window_artwork = shown;
    }

    /// Ask the system to suspend, restart or shut the machine down.
    fn session_action(&self, action: SessionAction) {
        if let Err(error) = (self.shared.session_actions)(action) {
            tracing::warn!(%error, ?action, "session action");
        }
    }

    /// Lock the session as the person asked: with a key, a command, or
    /// because one of their other devices locked. A session that cannot
    /// check a password never locks: the lock screen could not be left again.
    pub(crate) fn lock_by_person(&mut self) -> Result<(), Error> {
        if self.unlocking.verifier.available() {
            self.compositor.lock_now()
        } else {
            tracing::warn!("no password can be checked here: not locking");
            Ok(())
        }
    }

    fn command(&mut self, command: SessionCommand) -> Result<(), Error> {
        match command {
            SessionCommand::Lock => self.lock_by_person(),
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
            SessionCommand::OpenDisplaySheet => {
                self.display_sheet.open();
                Ok(())
            }
            SessionCommand::CloseDisplaySheet => {
                self.display_sheet.close();
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
                // process ends; without one, ending is the whole of it. A
                // session in a window of another desktop has no wrapper of
                // its own: ending is all logging out is there.
                if !self.shared.hosted
                    && let Err(error) = os_actions::request_logout()
                {
                    tracing::info!(%error, "no login wrapper to tell of the logout");
                }
                // Apps are told the session ends before the loop stops
                // answering them.
                if let Some(desktop) = &self.desktop {
                    desktop.end();
                }
                self.ended = true;
                Ok(())
            }
            SessionCommand::End => {
                self.ended = true;
                Ok(())
            }
            SessionCommand::NightLight { on } => {
                let (now, kept) = self.night_light;
                let on = on.unwrap_or(!now);
                self.night_light.0 = on;
                self.compositor
                    .set_night_light(on.then_some(NIGHT_LIGHT_WARMTH))?;
                if kept && let Err(error) = settings_store::hardware::NIGHT_LIGHT.set(on) {
                    tracing::warn!(%error, "the night light setting was not kept");
                }
                Ok(())
            }
            SessionCommand::PasteFiles { destination } => {
                self.shared
                    .clipboard
                    .paste_files(std::path::Path::new(&destination));
                Ok(())
            }
            SessionCommand::NextInputSource => {
                let Some(id) = self.sources.next() else {
                    return Ok(());
                };
                self.compositor
                    .set_keyboard_layout(&self.sources.compiled());
                if self.night_light.1
                    && let Err(error) = settings_store::update_section(
                        settings_store::hardware::INPUT_SECTION,
                        serde_json::Map::from_iter([(
                            "keyboard_layout".to_owned(),
                            serde_json::Value::String(id),
                        )]),
                    )
                {
                    tracing::warn!(%error, "the input source choice was not kept");
                }
                Ok(())
            }
            SessionCommand::AskUninstall { app } => {
                self.shared.app_sheets.ask_uninstall(&app);
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
        // A held direction repeats on its own clock, and a controller with
        // more waiting than one pass read is read again at once.
        let timeout = match self
            .controllers
            .as_ref()
            .and_then(|controllers| controllers.next_deadline(Instant::now()))
        {
            Some(due) => timeout.min(due.saturating_duration_since(Instant::now())),
            None => timeout,
        };
        self.compositor.dispatch(timeout)?;
        self.follow_controllers()?;
        self.follow_displays()?;
        self.follow_fullscreen();
        {
            self.shared.feed_agent_runs();
            let mut services = self.shared.services.borrow_mut();
            services.poll();
            if let Some(plane) = &self.action_plane {
                plane.link.serve(&mut services);
            }
        }
        // The surface apps are answered after the services took in what
        // their devices did, so a panel is sent the reading of this pass.
        self.surfaces.poll();
        self.surfaces.serve(&mut self.shared.doors);
        // Dictation writes into the field the person is typing in, so it runs
        // before the frame rather than with the sheets.
        let locked = self.compositor.locked();
        self.shared.dictation.step(
            &mut self.compositor,
            &self.shared.handle,
            &self.shared.clipboard,
            locked,
            Instant::now(),
        );
        mount::quick_settings::follow_dictation(&self.shared);
        // The picker's picks go into the field the person was typing in,
        // through the same text-input door dictation writes through.
        let origin = self
            .shared
            .desktop
            .with_widgets(|host| host.placement().origin);
        self.shared
            .emoji
            .step(&mut self.compositor, origin, Instant::now());
        let compositor = &mut self.compositor;
        self.shared
            .notifications
            .borrow_mut()
            .step(&mut || Some(compositor.activation_token()));
        self.shared.open_notifications();
        self.settle()?;
        self.follow_shell()?;
        // The toolkit's clock, which a capture pins the frame's instant on.
        let now = ui::time::now();
        self.windows.step(&mut self.compositor, now);
        // The overview's windows fly between the desk and the overview: its
        // panel names them, the desk says where each stands, and the
        // compositor draws their own pixels.
        self.shared.overview.step(&self.shared, now);
        self.compositor.render()?;
        // A live projection is sent the desktop as the pass just drew it.
        let video = self.display_sheet.video_stream();
        self.projection_video
            .follow(&mut self.compositor, video.as_ref());
        self.follow_shell()?;
        self.settle()?;
        self.shared.observe();
        self.shared.keep_desk();
        // The picture is the workspace in front's, which a change covering
        // one workspace lands on.
        self.shared
            .wallpapers
            .set_active(self.shared.handle.read(|tree| {
                tree.active_workspace()
                    .map(|workspace| workspace.id.0.to_string())
            }));
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
        self.shared.keep_desk_now();
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // The desk the person left, written whatever ended the session.
        self.shared.keep_desk_now();
    }
}

/// The apps that run now, in the states the portal asks of them: the app
/// in front is in use, the ones behind it only run.
fn running_apps(compositor: &Compositor) -> Vec<(String, u32)> {
    use tilo_portal::background::{ACTIVE, RUNNING};
    let focused = compositor.focused_window().and_then(|window| window.app_id);
    let mut apps: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::new();
    for (window, _) in compositor.windows() {
        let Some(app_id) = window.app_id else {
            continue;
        };
        let state = if focused.as_deref() == Some(app_id.as_str()) {
            ACTIVE
        } else {
            RUNNING
        };
        // An app showing more than one window is as in use as its best.
        let state = apps
            .get(&app_id)
            .copied()
            .map_or(state, |seen| seen.max(state));
        apps.insert(app_id, state);
    }
    apps.into_iter().collect()
}
