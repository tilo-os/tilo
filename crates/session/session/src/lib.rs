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
//! carried out by the ask loop in `asks`.
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

mod a11y;
mod a11y_settings;
mod activity;
mod activity_models;
mod activity_provider;
pub mod agent_door;
mod agents;
mod ambient_brightness;
mod app_icons;
mod app_liveness;
mod app_names;
mod app_sheets;
mod app_tiers;
pub mod arrangement;
mod asks;
mod automation;
mod bars;
mod bluetooth_handoff;
mod boot_join;
mod calls;
pub mod capture;
mod capture_grants;
mod capture_io;
mod capture_send;
mod capture_studio;
mod cards;
mod certification;
mod channels;
mod chrome;
mod clock_wait;
mod community;
mod config;
mod content;
mod continuations;
pub mod controller_nav;
pub mod controllers;
mod controls;
mod conversations;
mod desk;
mod desk_doc;
mod desk_migration;
mod desk_resource;
mod desk_restore;
pub mod desktop_services;
/// A Rust app's sample resources and views, in a development session only.
#[cfg(feature = "dev-rust-views")]
pub mod dev_rust_views;
mod device_actions;
mod devices;
mod diagnostics;
mod dictation;
mod directory;
mod display_confirm;
mod display_control;
mod display_identify;
mod display_settings;
mod display_sheet;
pub mod displays;
mod downloaded;
mod emoji;
mod engine;
mod env;
pub mod fabric;
mod feedback;
mod feeds;
mod file_clipboard;
pub mod files_daemon;
mod fs_authority;
mod held;
mod here;
mod host_actions;
mod i18n;
pub mod identity;
mod identity_admin;
mod import_readiness;
mod input_method;
mod input_sources;
mod invite;
pub mod keyboard;
mod keyboard_shortcuts;
mod keys;
mod launches;
mod lists;
pub mod live;
mod lock_screen;
mod lock_service;
mod look;
mod membership;
pub mod mount;
pub mod nearby_transfer;
mod network;
pub mod night_light;
pub mod notices;
mod notifications;
mod options;
pub mod overlays;
mod owner_session;
mod pairing;
mod panels;
mod pass;
mod people;
mod personal;
mod plane;
pub mod portal_link;
mod press_outside;
mod projection;
mod projection_video;
mod questions;
pub mod quick_settings;
mod record_index;
mod records;
pub mod recovery;
mod remote_door;
mod rotation;
pub mod run;
mod seams;
mod search;
mod search_ask;
mod seats;
pub mod seeded_apps;
mod send_to;
mod service_sessions;
mod session_actions;
pub mod session_package;
mod setup;
mod shared;
mod shared_desktop;
mod shared_displays;
mod sheets;
pub mod shell;
mod shortcut_guide;
mod slot_binding;
mod slot_views;
mod sockets;
pub mod startup;
mod status;
pub mod surfaces;
mod switcher;
pub mod sync;
mod system_sounds;
mod task_programs;
mod task_undo;
pub mod termination;
pub mod test_control;
pub mod unlock;
mod unlock_history;
mod update_finish;
mod wallpapers;
mod wifi_continuity;
mod window_motion;
mod window_move;
mod window_share;
mod window_transfer;
mod window_warmth;
mod workspace_delete;
mod workspace_gate;
mod workspace_settings;
mod workspaces;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use tilo_actions::remote::InvokeArgument;
use tilo_actions::{ActionAddress, DoorError, ResourceSpec};
use tilo_compositor::{Compositor, DisplayConfig, Error, NestedConfig, OutputConfig, WindowPolicy};
use tilo_desktop::chrome::ChromeIntent;
use tilo_desktop::desktop_host::DesktopAsk;
use tilo_desktop::osd::{Hint, Osd, OsdKind};
use tilo_desktop::overlays::{Overlays, Slot};
use tilo_desktop_surface::DesktopHandle;
use tilo_lock::MutexRecover as _;
use tilo_service::Started;
use tilo_surface_host::{DesktopHost, Doors, WidgetHost};
use ui::host_operation::ResourceOperationStart;
use ui::shortcut_table::ShortcutAction;

pub use automation::FinishedShortcutRun;
pub use bars::BarShown;
pub use channels::ChannelLink;
pub use clock_wait::ClockWait;
pub use config::SessionConfig;
pub use controls::{
    CLIPBOARD_RESOURCE, INPUT_RESOURCE, MAGNIFIER_RESOURCE, Magnify, NIGHT_LIGHT_RESOURCE,
    SESSION_RESOURCE, SessionCommand,
};
pub use desk_migration::shell::shell_state_path;
pub use devices::RESOURCE as DEVICES_RESOURCE;
pub use engine::Person;
pub use env::SessionEnvironment;
pub use keys::{Invocation, KEYBOARD_BACKLIGHT, MEDIA, MICROPHONE, QUICK_SETTINGS};
pub use launches::Launches;
pub use lists::{AppLists, Lists};
pub use mount::vpn::Vpn;
pub use mount::wake::{Seams as WakeSeams, WakeInterfaces};
pub use notifications::RESOURCE as NOTIFICATIONS_RESOURCE;
use options::start_collaboration;
pub use options::{ActionPlane, Machine, Options, no_session_actions};
pub use records::Records;
pub use rotation::Accelerometer;
pub use run::{Backend, Reach};
pub use search::{AppActions, SearchOptions};
pub use setup::BeforeDesk;
pub use tilo_service::{Service, Services};

use crate::mount::panels::mount_activities;
use desk::running_apps;
use keys::{GuideKeys, Invocations, PressedChords};
use mount::panels::{KeyContext, mount_controls, mount_setup};
use mount::services::{mount_services, start_action_plane};
use notifications::mount_banners;
use panels::Held;
use plane::Plane;
use search::SessionSearch;
use setup::Setup;
use tilo_app_launch::Doors as WindowDoors;
use tilo_sheets::delete_workspace::DeleteWorkspaceSeat;
use tilo_sheets::directory::DirectorySeat;
use tilo_sheets::display_confirm::ConfirmSeat;
use tilo_sheets::file_picker::FilePickerSeat;
use tilo_sheets::invite::InviteSeat;
use tilo_sheets::portal::PortalSeat;
use tilo_sheets::power::PowerSeat;
use tilo_sheets::workspace_settings::WorkspaceSettingsSeat;

pub use os_actions::SessionAction;

pub use tilo_onboarding_host::view::OnboardingView;

/// The generation the session's own desktop snapshots carry: a snapshot of
/// an older one is stale.
const GENERATION: &str = "session";

/// How many times the session starts the lock screen's app for one lock
/// before it draws the lock itself: a screen whose app goes is started
/// again, and one that keeps going is not staying up at all. The lock stays,
/// and the session's own view of it takes over, because a lock is never
/// lifted for anything but the right password.
const LOCK_SCREEN_TRIES: u32 = 3;

/// Where each published part stood when search last read it: the part's
/// index and its revision and head.
type Observed = RefCell<Option<Vec<(usize, (u64, u64))>>>;

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
    /// The ids they went by before this device had an identity, which their
    /// own workspaces may still name them by.
    earlier: Vec<String>,
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
    /// Whether the shell's send key was pressed, waiting for the pass that
    /// asks the window in front to open its box. The key can arrive while the
    /// desk is between two of its own frames, so the window it was over is
    /// named then, by the desk, and never at the keypress.
    send_from: Cell<bool>,
    /// Whether the shell's "report a problem" was asked for, waiting for the
    /// pass that asks the window in front to open its box to the one place
    /// this computer sends reports; and where that place is, when this
    /// computer is told.
    report_from: Cell<bool>,
    feedback: Option<feedback::Destination>,
    /// Where the chrome takes intents the session raises.
    intents: Rc<RefCell<Vec<ChromeIntent>>>,
    /// Whether this session shows quick settings at all: its app is one of the
    /// surface apps it runs, and the bar's status keys open its slot.
    quick_settings: bool,
    /// What the keys asked the panel to show before it ran — the notifications,
    /// the clipboard opened to paste, the page Escape goes back from — waiting
    /// for its app to be up.
    panel_asks: Rc<RefCell<Vec<&'static str>>>,
    /// The ask the panel goes back on, while it has not answered whether it
    /// had anywhere to go back to: Escape closes the panel only from its main
    /// page, and the panel says which page that is.
    panel_back: Cell<Option<u64>>,
    /// What stands over the windows: its menus, quick settings, the picker
    /// and the rest, and which of them holds the keyboard.
    overlays: Overlays,
    /// The two stacks the surfaces stand in, as one: what the shell's views
    /// and the desk's own popovers open through, so one of them closes the
    /// others.
    surfaces: crate::desk_resource::Surfaces,
    /// The desk's own overlays, as its reading and its views name them: the
    /// surface up, and Alt+Tab's walk.
    desk_overlays: Rc<overlays::Overlays>,
    /// Alt+Tab's panel, shared by the desk's walk (which sets it), the chrome
    /// (which draws it) and the compositor (which pictures its windows).
    switcher: tilo_desktop::window_switcher::Switcher,
    /// Every hosted surface's and service's published side: the session's
    /// action host.
    doors: Doors,
    /// Published actions keys and gestures asked for, waiting for the door.
    invocations: Invocations,
    /// Chords pressed that run a saved shortcut, waiting for the automation
    /// host.
    triggered: PressedChords,
    /// What the session's own resources asked of the compositor.
    commands: Rc<RefCell<Vec<SessionCommand>>>,
    /// What the session's own resources read of this device.
    controls: Rc<RefCell<controls::ControlsReading>>,
    /// The readouts of keys whose action a device took and has not answered
    /// yet.
    waiting: RefCell<Vec<keys::PendingReadout>>,
    /// What the services last published of the machine: its connection, its
    /// sound and capture uses, its charge and what plays. The bar's status
    /// cluster, the lock screen and the desk read it.
    readings: status::Shared,
    /// The system services, reached for their readings.
    services: Rc<RefCell<Services>>,
    /// The services the session serves its apps itself.
    table: mount::doors::Doors,
    /// The windows this session shares with its workspaces, and the streams it receives from them.
    window_shares: Rc<RefCell<window_share::WindowShares>>,
    /// The turns this session's agents answer: a person's message in a
    /// conversation the agent takes part in, read every pass from Chat's
    /// record of each workspace.
    conversations: std::rc::Rc<conversations::Conversations>,
    /// The agents each workspace keeps, read every pass from its record of
    /// them: who keeps an agent is that record's word, and what the broker
    /// admits an agent's session on.
    agents: std::rc::Rc<agents::Agents>,
    /// The agents the person admitted at its agent door, as the desk and the bar show them.
    seats: std::sync::Arc<agent_door::Seats>,
    /// It runs in a window of a desktop it did not start (see
    /// [`Options::machine`]).
    hosted: bool,
    /// The person's device fabric, while this session runs on one.
    fabric: Option<tilo_device_fabric::link::SharedLink>,
    /// The device VPN, while this session runs on a machine with the helper:
    /// its profiles' kill switches, and the route through another of the
    /// person's devices, which follows the pass.
    vpn: Option<mount::vpn::Routes>,
    /// The person's devices as the panel was last fed them.
    devices: mount::devices::Devices,
    /// Moving a Bluetooth accessory between the person's devices: what the
    /// panel asked for, what their other devices said, and the Bluetooth
    /// work the move needs.
    bluetooth: bluetooth_handoff::Handoffs,
    /// The session another of the person's devices handed this one, as the
    /// now-playing card shows it, until they continue it here: published
    /// beside the person's devices.
    handoff: Rc<std::cell::RefCell<Option<tilo_quick_settings::MediaOffer>>>,
    /// The wakes this session takes part in.
    waking: Rc<mount::wake::Wake>,
    wake_handle: tilo_service::Wake,
    /// What plays, as the desk's music widget shows it.
    music: tilo_desktop::desk_widgets::DeskMusic,
    /// The power menu, workspace settings, the questions the session asks
    /// for apps and services, and the one deleting a workspace asks, by
    /// their seats.
    power: Option<PowerSeat>,
    workspace_settings: Option<WorkspaceSettingsSeat>,
    /// The desk revision workspace settings was last fed at: its panes are
    /// rebuilt when the desk changed under it.
    ws_settings_fed: Cell<Option<u64>>,
    /// The desk revision the file access list and what apps may capture were
    /// last worked out at: both follow every change of the desk, so a
    /// revocation reaches the running app at once.
    capabilities_fed: Cell<Option<u64>>,
    /// What publishes the person's file access list to the Files daemon
    /// while the session runs; `None` where no Files workspace serves.
    fs_authority: Option<fs_authority::Publisher>,
    /// The workspace's agents and emoji, which workspace settings shows and
    /// changes through Chat's record of the workspace.
    workspace_tabs: workspace_settings::WorkspaceTabs,
    portal: Option<PortalSeat>,
    file_picker: Option<FilePickerSeat>,
    /// The question about a display change, which stands on every display
    /// at once so it is seen even when the change blanked one of them.
    display_confirm: Option<ConfirmSeat>,
    /// The workspaces this computer may join or open.
    directory: Option<DirectorySeat>,
    /// Inviting people: the sheet a link into the workspace in front is made
    /// from, and what it made in this opening.
    invite: Option<InviteSeat>,
    invite_made: RefCell<tilo_sheets::invite::Making>,
    /// The question deleting a workspace asks, and the workspace it is about.
    delete_workspace: Option<DeleteWorkspaceSeat>,
    delete_workspace_ask: RefCell<Option<tilo_desktop_model::WorkspaceSlot>>,
    /// What the sites this computer knows list as public workspaces, read
    /// on a worker of its own.
    directory_store: directory::Store,
    /// The workspace the host started this session to join.
    boot_join: RefCell<boot_join::BootJoin>,
    /// The community site this computer is configured with, joined on its
    /// own as the session starts.
    community: RefCell<community::Community>,
    /// The lock, as the session publishes it for the lock screen to show.
    lock: lock_service::Lock,
    /// The on-screen keyboard, as the session publishes it for its app to
    /// show where the person types.
    keyboard: keyboard::Keyboard,
    /// This device's side of the person's session on their devices: what it
    /// says of its own lock and what it has followed from theirs.
    owner_session: owner_session::SessionFollow,
    /// The desktop's own notices: what the session tells the person, such as
    /// leaving a workspace.
    notices: tilo_desktop_surface::DesktopNotifications,
    /// The windows carried to another workspace, asked about first.
    moves: window_move::Moves,
    /// The apps' live activities on the bar, which the pill names and the
    /// controls pressed on it go back to the app through.
    activities: Rc<chrome::Activities>,
    /// The running applications' tray items, which the bar shows and whose
    /// clicks the session hands back to the applications.
    tray: Option<std::sync::Arc<tilo_tray_host::Tray>>,
    /// The pictures the desk shows, by workspace.
    wallpapers: std::sync::Arc<wallpapers::Wallpapers>,
    /// The keyboard layouts the person switches between, and which one is
    /// in use, as the lock screen shows them.
    lock_layouts: Vec<String>,
    lock_layout: Cell<usize>,
    session_actions: fn(SessionAction) -> Result<(), String>,
    /// Where everything the session publishes stood when search last read
    /// it.
    observed: Observed,
    /// The second search last read who is here: people and the calls they are
    /// in change with the clock as well as with what arrives.
    here_at: Cell<u64>,
    /// What presence stood at when search last read who is here, so a person
    /// arriving inside that second is listed at once.
    here_seen: Cell<u64>,
    /// The title each window had when search last recorded where it was.
    places_seen: RefCell<std::collections::HashMap<u64, String>>,
    /// The daemon's notifications on the desktop, as the session publishes
    /// the centre they are kept in.
    notifications: Rc<RefCell<notifications::SessionNotifications>>,
    /// What the services ask the person, on the sheets.
    asker: questions::Asker,
    /// Who is here: this session's membership of each workspace, when it
    /// takes part in the collaboration plane.
    sync: Option<Rc<sync::Client>>,
    /// This device's workspaces: what the desk shows of them, and what a join takes in.
    engine: Option<std::sync::Arc<engine::Engine>>,
    /// The workspaces' shared desktops, as their records hold them: what each
    /// device that shows a workspace puts on its own desk, and what this one
    /// writes back.
    desktops: shared_desktop::SharedDesktops,
    /// What each window this session runs shows, read from the broker every
    /// pass: the desk's reading of it, and the record noted beside the window
    /// in its workspace's layout.
    notes: std::sync::Arc<continuations::Continuations>,
    /// The engine revision the desk's copy of each workspace's people was
    /// last taken from: what nothing new of a workspace's governance has
    /// passed since.
    membership_fed: Cell<Option<u64>>,
    /// This device's channels of those workspaces' sealed records, and the
    /// devices each one admits: kept for as long as the session runs.
    _channels: Option<channels::Channels>,
    /// The person's own workspaces, as all their devices hold the list of
    /// them: `None` when this computer has no identity to keep it for.
    personal: Option<RefCell<workspaces::Personal>>,
    /// The copies the person can paste again.
    clipboard: Rc<mount::clipboard::Clipboard>,
    /// The person's arrangement: the desk this session holds, with history and
    /// undo (shell.md §2), and the app places its windows from it.
    arrangement: Rc<RefCell<arrangement::Arrangement>>,
    /// Whether the person is mid-gesture or typing, as the compositor last
    /// said: what an arranging action that is not their own waits for.
    underway: Rc<Cell<arrangement::Busy>>,
    /// The person's settings, kept in step with their other devices.
    settings: mount::settings_sync::SettingsSync,
    /// A joined device's certificate, renewed through the person's devices
    /// once it is due: this device holds no root to reissue it with.
    fleet_renewal: Option<std::sync::Arc<mount::renewal::FleetRenewal>>,
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
    /// Whether the person may invite someone into the workspace in front, as
    /// the loop last answered it: the bar's invite button and the roster's
    /// footer are offered on it.
    invitable: Rc<Cell<bool>>,
    /// The person's "hide the top bar until the pointer is near it", as the
    /// settings watch last read it.
    hide_top_bar: std::sync::Arc<std::sync::atomic::AtomicBool>,
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
    /// What a connection of any door asks of the session's own doors: answered
    /// on the loop's pass, the only place that may touch them.
    session_doors: std::sync::Arc<agent_door::SessionDoors>,
    /// The agent door toward the person's other devices.
    remote: std::sync::Arc<remote_door::Remote>,
    /// The installed folder importer's side of this login, once the session
    /// serves it: what it asks on whether first use is over, and the review
    /// it saved to be opened when it is.
    folder_import: RefCell<Option<import_readiness::Continuation>>,
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
    /// The first frame reached the display: the startup is measured and
    /// closed.
    first_frame: bool,
    /// The workspace in front, as this pass last read it: a change is a
    /// switch to time.
    active_workspace: Option<u64>,
    /// The session's own actions, as the broker's steps: what each pass hands the rows it read.
    published_steps: Option<std::sync::Arc<session_actions::SessionActions>>,
    /// When the desk's own bookkeeping last read it.
    kept_at: Instant,
    /// What the desk's input a tool handed the loop cost this pass, read with
    /// the control socket's own time in the frame's timings.
    tool_input: std::time::Duration,
    /// The magnifier's zoom level and the one its toggle turns on at.
    magnifier: (u8, u8),
    /// Setup, while it runs: it withholds the desktop's actions.
    setup: Option<Rc<RefCell<Setup>>>,
    /// The login screens' comforts, kept equal to the person's own.
    comforts: Option<mount::comforts::Mirror>,
    /// Checking the passwords the lock screen was given.
    unlocking: Unlocking,
    /// How many times the session started the lock screen's app for the
    /// lock in force: a screen that keeps going is not staying up at all,
    /// and the session draws the lock itself from then on.
    lock_starts: u32,
    /// The lock drawn in this process, for the locks the lock screen's app
    /// cannot show.
    lock_view: Option<lock_screen::LockView>,
    /// What shows the lock in force: the lock screen's app, or, when that app
    /// cannot show it, [`Session::lock_view`] — nothing but this process then
    /// draws and unlocks it.
    lock_shown: lock_screen::LockShown,
    /// The login's lock record: the state this session ends in is published
    /// to it, and a session that ends without doing so comes back locked.
    recovery: Option<recovery::Recovery>,
    /// The clock on finishing the staged system update.
    update_finish: update_finish::Finish,
    /// The screenshots asked for.
    captures: Option<capture::Captures>,
    /// The one worker that encodes a recording and writes every capture's
    /// file, off the loop.
    capture_io: Rc<capture_io::CaptureIo>,
    /// The capture studio the session shows, and answers the asks of.
    capture_studio: Option<capture_studio::CaptureStudio>,
    /// The windows opening and closing.
    windows: window_motion::WindowMotions,
    /// The quick settings panel coming and going, which the compositor draws.
    quick_settings_panel: quick_settings::PanelMotion,
    /// The temperature and colour every app window is told, as the settings
    /// and wallpapers last said them.
    warmth: window_warmth::Windows,
    /// The language the session's own surfaces speak, as the person's
    /// settings last said it.
    language: mount::language::Language,
    /// The person's warmth and when it shows, and the panel following the
    /// light in the room while they asked for it.
    night_light: night_light::NightLight,
    auto_brightness: ambient_brightness::AutoBrightness,
    /// Where each display stands and where the person types, as the readout
    /// answers with them.
    readout_place: crate::panels::ReadoutPlaces,
    /// The readout itself, so a theme the person changed reaches it.
    readout: Rc<RefCell<crate::panels::Readout>>,
    /// When the on-screen keyboard runs, as the person chose: never, over a
    /// field they touch, or always.
    keyboard_preference: tilo_accessibility_settings::OnScreenKeyboard,
    /// Where the keyboard's app stands, while the person may want it: a
    /// keyboard on another display, or on one that changed size, is started
    /// again where they type.
    keyboard_run: keyboard::Run,
    /// How often the keyboard's app was started on the display it stands on,
    /// so one that keeps exiting is given up on rather than started for ever.
    keyboard_starts: u32,
    /// Where quick settings' app stands, and how often it was started at
    /// that place: one that keeps exiting is given up on.
    quick_settings_run: quick_settings::Run,
    quick_settings_starts: u32,
    /// The portal's link to the loop, while the portal serves.
    portal: Option<portal_link::SessionLink>,
    /// The apps this session started, told to the portal: which of them run
    /// in one of the launcher's sandboxes.
    launches: std::sync::Arc<Launches>,
    /// The plane the apps it runs read and invoke its services on, while it
    /// serves one.
    action_plane: Option<Plane>,
    /// The content its workspaces' records reference, and what a window it
    /// does not run is pictured into: the address is read by any caller of
    /// the action protocol.
    content: std::sync::Arc<content::Plane>,
    /// The control door `tilo-control` drives this session over: the
    /// development and the device admin's sockets, when the launcher names
    /// them, and the one a program inside an app reaches.
    control: test_control::Control,
    /// The liveness door beside the development control socket: whether the
    /// loop still turns, and where its threads are when it does not. Kept for
    /// as long as the session runs: dropping it takes the door down.
    #[allow(
        dead_code,
        reason = "held for the session's life: dropping it takes the door down"
    )]
    liveness: Option<tilo_liveness::Server>,
    /// The shortcuts the person saved, started by their triggers.
    automation: automation::Automation,
    /// The runtime the shortcuts' runs execute on, which the apps' broker
    /// starts them on. Kept for the session's life: dropping it stops the
    /// record feed and the runs still executing on it.
    automation_runtime: tokio::runtime::Runtime,
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
    /// The views the person bound to the shell's slots, and the one running
    /// each slot now.
    slots: mount::slots::Slots,
    /// The slots whose view is a tilolang package, which the session runs
    /// itself.
    slot_views: slot_views::SlotViews,
    /// The displays as the person arranged them, kept in step with the ones
    /// attached.
    displays: display_settings::Displays,
    /// The displays as a published service: the arrangements they are used in
    /// and how bright each one is, kept told of the outputs the compositor
    /// shows.
    displays_service: displays::Displays,
    /// Settings' Identify answers: which screen is which, said on the
    /// screens.
    numbers: display_identify::Numbers,
    /// The person's screens across their devices, as this session offers its
    /// own and publishes what every device shows.
    published_displays: shared_displays::Publisher,
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
    /// The composing input method of the active source, while one runs.
    input_method: input_method::InputMethod,
    /// The machine's power: the idle policy, the service over logind and
    /// what holds it awake.
    machine: mount::power::PowerRun,
    /// The shell's bar, when its package started.
    shell: Option<shell::Shell>,
    /// What assistive technology reads of the session's own surfaces: the
    /// desk, the bar, the switcher, the panels and the sheets, each a window
    /// of the session's AT-SPI application.
    a11y: a11y::Accessibility,
    /// The shell-ui socket the apps ask about their own tabs on, while it
    /// serves.
    tabs: Option<mount::app_tabs::AppTabs>,
    /// The controllers in the room, and how they drive the desktop; `None`
    /// when this session reads none.
    controllers: Option<controllers::Controllers>,
    connectors: mount::connectors::ConnectorHost,
    /// The controllers' descriptors, polled by the loop so their input ends
    /// its wait, by the key the controller source gave each.
    controller_wakes: std::collections::BTreeMap<u64, tilo_compositor::ReadableWatch>,
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
    #[expect(
        clippy::too_many_lines,
        reason = "the session is wired in one pass, where every part sees the others"
    )]
    pub fn new(
        mut options: Options,
        start: impl FnOnce(Box<dyn WindowPolicy>) -> Result<Compositor, Error>,
    ) -> Result<Self, Error> {
        tilo_startup_metrics::mark("session-build-start");
        // The words a notice or a query resolves later, in the person's
        // language, are read now: the loop that shows them reads no file.
        i18n::ensure();
        tilo_startup_metrics::mark("session-i18n-done");
        // Startup frees its scratch at no single moment: one trim collects it
        // once startup is over, and the log says where the heap stood at the
        // start.
        tilo_compositor::heap_trim::log_milestone("start");
        tilo_compositor::heap_trim::request_after(tilo_compositor::heap_trim::STARTUP_SETTLED);
        // Who sits here, as the one id the session acts as: the person their
        // workspaces' governance names, which this device's certificate
        // carries. Everything the person does here — the workspaces they
        // own, what the desk records, what the sheets change — is theirs.
        let seat = identity::seat(&options);
        options.person = seat.person;
        // The ids this device mints are its own, as every workspace derives
        // them from its key: a computer with an identity mints under that
        // rather than the seed the caller passed.
        if let Some(salt) = seat.salt {
            options.salt = salt;
        }
        let sources = config::Sources::from_config(&options.config);
        let mut doors = Doors::default();
        // The device VPN runs for the session's lifetime: the profiles' kill
        // switches from the start, the route through another of the
        // person's devices as the session's passes find this device's
        // endpoint, and this device as an exit for their other devices. What
        // all of it does is published for Settings, and the exit is served
        // on the endpoint the session binds next.
        let beside = options
            .vpn
            .as_ref()
            .map(mount::vpn::Vpn::served)
            .unwrap_or_default();
        let vpn = options.vpn.take().map(mount::vpn::Vpn::start);
        // What the desk and the chrome ask of the session, from their own
        // widget trees.
        let notices = tilo_desktop_surface::DesktopNotifications::default();
        // The centre opens with what it kept when this session last ended.
        let notice_history = notifications::restore_history(&notices);
        // What the bar reads of the notifications, and the policy arrivals
        // are judged by: the person may change it at any time.
        let notice_signals = Rc::new(notifications::NoticeSignals::default());
        let notification_policy = std::sync::Arc::new(std::sync::Mutex::new(
            settings_store::notification_policy::NotificationPolicy::load(),
        ));
        let watched = std::sync::Arc::clone(&notification_policy);
        let woken = options.wake.handle();
        // A device's answer to another's ask about the person is worked out
        // off the loop; when it lands here, the loop wakes to put it to them.
        crate::certification::attach(options.wake.handle());
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
        // What may continue on the person's other devices — what they play
        // here, the Wi-Fi passphrases they share — read from the same settings
        // document on the same watch.
        let continuity = fabric::Continuity::default();
        continuity.read(&settings_store::load());
        let watched_continuity = continuity.clone();
        // The bar's own setting, read on the same watch: it changes what the
        // desk's band takes and when the bars show.
        let hide_top_bar = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
            settings_store::desktop_controls::DesktopControlsConfig::load().hide_top_bar,
        ));
        let watched_hide = std::sync::Arc::clone(&hide_top_bar);
        // The night light's warmth and schedule, and whether the panel
        // follows the light in the room, read on the same watch.
        let (night_light, watched_night_light) =
            night_light::NightLight::watched(options.config.night_light, options.config.kept);
        let (auto_brightness, watched_auto) = ambient_brightness::AutoBrightness::watched();
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
            watched_continuity.read(&document);
            watched_night_light.read(&document);
            ambient_brightness::read(&watched_auto, &document);
            watched_hide.store(
                settings_store::desktop_controls::DesktopControlsConfig::from_document(&document)
                    .hide_top_bar,
                std::sync::atomic::Ordering::Relaxed,
            );
            woken();
        })
        .map_err(|error| tracing::warn!(%error, "settings are not watched"))
        .ok();
        let presentation = (options.presentation)();
        tilo_startup_metrics::mark("session-presentation-done");
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
        // The workspace's activity: the desk's own document, and the records
        // its apps keep in the store. The desk records into it, the model
        // reads it, and the apps this session runs reach the same model.
        let activity = activity::Activity::new(
            options.state.as_deref(),
            options.old_desk.as_deref(),
            options.person.clone(),
            options.search.records.as_ref().map(records::Records::root),
            options.search.lists.clone(),
            Some(options.wake.handle()),
        );
        tilo_startup_metrics::mark("session-activity-done");
        // When each display's bar shows over the window covering it: the desk's
        // band, the chrome and the input that reaches for the bar read the one
        // model.
        let reveals: Rc<RefCell<tilo_desktop::topbar::Reveals>> = Rc::default();
        // The desk a login shows is built before its first frame: this pair
        // is the phase between the compositor coming up and the frame that
        // draws it (tools/perf/startup_marks.py).
        tilo_startup_metrics::mark("desk-build-start");
        let (desktop, strips, asks) = desk::desk(
            &options,
            &activity,
            notices.clone(),
            std::sync::Arc::clone(&wallpapers),
            desk::DeskLook {
                widgets: widget_prefs,
                fold: fold.clone(),
                overview: overview.open_flag(),
                music: music.clone(),
                reveals: Rc::clone(&reveals),
            },
        );
        desktop.set_gestures(options.config.gestures);
        // What each open window is told of the desk it stands on, from the
        // settings as they are now; the desk wears the same theme.
        let warmth = window_warmth::Windows::new(presentation.theme.clone(), &options.person);
        desktop.set_theme(presentation.theme.clone());
        tilo_startup_metrics::mark("desk-build-done");
        let handle = desktop.with_policy(|policy| policy.handle().clone());
        // The desk as this device left it: its workspaces, the windows they
        // held, and the apps those windows open again.
        let mut restore = desk_restore::Restore::default();
        match activity.opened_desk() {
            desk_doc::Kept::Desk(kept) => {
                let mut kept = *kept;
                // A launch that no window came for is not a window this start
                // restores, and keeping it would leave one more of them on the
                // desk with every restart.
                desk_restore::without_unfilled(&mut kept);
                restore = desk_restore::Restore::of(&kept);
                handle.replace_tree(kept);
            }
            // The person's desk is on disk and this session did not open it:
            // they are looking at a desk that is not theirs, and have to be
            // told where their own still is.
            desk_doc::Kept::Refused(refusal) => {
                notices::tell_desk_not_opened(&notices, &refusal);
            }
            desk_doc::Kept::None => {}
        }
        // What each window shows, as the broker knows it: the desk offers the
        // windows a person may share from, and the sharing doors read it.
        let notes = options.launches.notes();
        // The person's arrangement: the desk this session holds, which the
        // workspace shares as its own desk record. A device that kept its
        // arrangement beside the desk before this build has what it holds
        // opened on the desk, once.
        let arrangement = arrangement::Arrangement::new(
            &options.person,
            handle.clone(),
            activity.clone(),
            options.state.as_deref(),
        );
        let arrangement = Rc::new(RefCell::new(arrangement));
        // Whether the person is mid-gesture or typing, as only the
        // compositor knows: what an arranging action that is not their own
        // reads before it moves anything.
        let busy = Rc::new(Cell::new(arrangement::Busy::default()));
        let underway = Rc::clone(&busy);
        // The one search text typed on the canvas places through: the
        // session search's own, the same `find` the palette and an agent's
        // `tilo` tool run.
        // The search is made further down, from what the session mounts in
        // between; it is in place before the session runs any action.
        let session_search: Rc<std::cell::OnceCell<Rc<SessionSearch>>> = Rc::default();
        let find: arrangement::Find = {
            let search = Rc::clone(&session_search);
            Rc::new(move |needle: &str| {
                search
                    .get()
                    .map(|search| search.find(needle))
                    .unwrap_or_default()
            })
        };
        // What text that asks does (shell.md §8.1, "Asking from the canvas"):
        // the session's own ask, which reads Chat's record, the workspace in
        // front and its agents. It is answered here and reads what the
        // session mounts further down, so it is late-bound the way the search
        // is: the door holds the seam, and the closure looks the session up
        // by the cell it was given at each call.
        // The conversations this session's agents answer in, and the agents
        // each workspace keeps, both read from Chat's record of them: made
        // here, before the door, because what text that asks does reads them
        // and the desk in front (shell.md §8.1, "Asking from the canvas").
        let conversations = Rc::new(conversations::Conversations::new(
            cli::data_home(),
            options.wake.handle(),
        ));
        let agents = Rc::new(agents::Agents::new(cli::data_home(), options.wake.handle()));
        let ask: arrangement::Ask = {
            let handle = handle.clone();
            let person = options.person.clone();
            let conversations = Rc::clone(&conversations);
            let agents = Rc::clone(&agents);
            let catalog = arrangement.borrow().targets();
            Rc::new(move |text: &str| {
                let (workspace, policy) = handle.read(|tree| {
                    tree.active_workspace()
                        .map(|held| (held.id().to_string(), held.policy.clone()))
                })?;
                let agent = agents.keep_one(&workspace, &policy, &person)?;
                let channel = conversations.ask(&person, &workspace, &policy, &agent, text)?;
                // A channel the ask just made is a resource the arrangement may
                // place the moment it is asked to: search has not read Chat's
                // record yet, so the place's app and default view are put where
                // the catalog is read from besides the door's own tree
                // (`Catalog::remember`), and the next read of the door's
                // resources carries it like any other.
                catalog.borrow_mut().remember(
                    &channel,
                    arrangement::Target {
                        app: "so.tilo.chat".to_owned(),
                        views: Vec::new(),
                        title: crate::i18n::t("session-ask-conversation"),
                    },
                );
                Some(channel)
            })
        };
        doors.add(Rc::new(RefCell::new(arrangement::ArrangementDoor::new(
            Rc::clone(&arrangement),
            Rc::new(move || underway.get()),
            find.clone(),
            ask,
        ))));
        // What the person's settings say the desk shows: a device that kept
        // no picture yet takes the choice, and otherwise the workspaces keep
        // the pictures this device kept for them until the setting changes.
        wallpapers.set_active(handle.read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.slot.0.to_string())
        }));
        wallpapers.start(&settings_store::load());
        // The apps the person opens read their workspace from this desktop, leave
        // their notice in it, and act as the person the workspaces' governance
        // names.
        if let Some(launch) = &options.launch {
            launch.attach(desktop.clone());
            launch.set_person(options.person.clone());
        }
        // One process per app per workspace: its later windows open in the
        // process the session started, with connections of their own. A
        // window mapping settles what its app's door serves, so an app built
        // before the door keeps a process per window from then on.
        let window_doors = WindowDoors::new(mount::app_tabs::ServiceLinks::new(
            std::sync::Arc::clone(&options.launches),
        ));
        let settling = Rc::clone(&window_doors);
        let mut policy = desk::DeskPolicy::for_shell(options.shell, &desktop, strips);
        // What each window shows: the desk offers the windows a person may
        // share from the note, and one whose app holds no record shows none.
        desktop.with_policy(|policy| {
            policy.reads_window_records(Rc::new(continuations::DeskNotes(std::sync::Arc::clone(
                &notes,
            ))));
        });
        let wells = policy.wells();
        policy.set_window_mapped(Rc::new(move |pid| settling.window_mapped(pid)));
        if let Some(launch) = &options.launch {
            launch.lend_doors(Rc::clone(&window_doors));
        }

        // The programs the person downloaded: the registry takes the
        // windows the desk gives a tab as they appear, so one that names no
        // application directory is offered the first time it shows itself.
        downloaded::install(&mut options, &mut policy);
        tilo_startup_metrics::mark("compositor-start");
        let mut compositor = start(policy.boxed())?;
        // The render context is up: every mark from here is the frame the
        // person waits for (tools/perf/startup_marks.py).
        tilo_startup_metrics::mark("window-render-context-done");
        // A session in a window of another desktop runs no idle timers: that
        // desktop owns idle, blanking, locking and suspend.
        compositor.set_hosted(!options.machine.is_owned());
        // What the services take in wakes the loop instead of waiting for
        // its next pass.
        options.wake.bind(compositor.waker());
        compositor.set_input_config(options.config.input.clone());
        controls::apply_keyboard(&mut compositor, &options.config.keyboard);
        compositor.set_idle_config(options.config.idle);
        a11y_settings::apply(&mut compositor, &options.config.a11y)?;
        compositor.set_backlight(Box::new(display_settings::PanelBacklight::find()));
        let machine = mount::power::install(&mut compositor, &options);
        compositor.add_host(shell::under(options.shell, desktop.clone()));
        // The desk draws every window whole — its card, the lift under it
        // and its edge, in the theme's colours, brighter on the window in
        // use. The compositor shapes a window's corners to that card and
        // draws nothing around it: one drawing, not two edges and two
        // shadows on the same pixel.
        compositor.set_window_frame(tilo_compositor::WindowFrame::corners(f64::from(
            presentation.theme.tokens.radii.tile,
        )));
        // What stands over the windows, opened by the chrome's own widgets, by the session's
        // verbs and by the views bound to them, and read by whoever asks through the door.
        let overlays = Overlays::new();
        let desk_overlays = Rc::new(overlays::Overlays::new());
        // Alt+Tab's panel: the desk's walk says what it shows, the chrome draws it, and the
        // compositor pictures the windows its cards name.
        let switcher = tilo_desktop::window_switcher::Switcher::default();
        // The surfaces the desk shows over its windows, and where each stands: the shell's own
        // views, or the quick settings' app where the device has one and the desk draws it. The
        // desk's door and the chrome's door open them through it.
        let surfaces = desk_resource::Surfaces {
            status_panel: quick_settings::installed(&options.surfaces),
            shell: options.shell,
            panel: overlays.clone(),
            overlays: Rc::clone(&desk_overlays),
            overview: overview.open_flag(),
        };
        // The same handle, for the pass that keeps one surface up at a time.
        let one_surface = surfaces.clone();
        // What the keys ask the panel's own app, which the surface loop hands
        // the running app each pass. A device whose desk draws the panel has
        // one; the shell's own view of the quick settings answers its own keys.
        let panel_asks: Rc<RefCell<Vec<&'static str>>> = Rc::default();
        let panel_keys =
            (surfaces.panel_shows(quick_settings::SURFACE)).then(|| Rc::clone(&panel_asks));
        let desk_door: tilo_surface_host::Shared =
            shell::desk_door(options.shell, desktop.clone(), surfaces.clone());
        doors.add(Rc::clone(&desk_door));
        // The chrome above the windows: its own tree, beside the desk's.
        let intents = Rc::new(RefCell::new(Vec::new()));
        let engine = activity.open_engine(&options, &handle);
        // This device's channels of its workspaces' sealed records: its keys,
        // and the devices each one admits, which its window streams carry
        // between. The broker the apps reach is the one the channel service
        // gates, so a sealed record opens before any of it reaches an app.
        let channels_broker = std::sync::Arc::new(std::sync::Mutex::new(
            tilo_action_broker::collab_core::BrokerChannels::default(),
        ));
        // What the broker shows the apps that hold a record's route open: the
        // collaboration plane hands it what another device wrote, so a record
        // an app holds shows everyone's writes, and the broker below shows
        // this device's own.
        let channels_fanout =
            std::sync::Arc::new(tilo_action_broker::collab_broker::FanoutState::new());
        let channel_link = channels::ChannelLink::new();
        // Search reads the workspaces' records off the records themselves,
        // with no window of their app running: what this device holds is
        // found before anything changes, and every change after it.
        let record_index = engine.as_ref().map(|engine| {
            crate::record_index::RecordIndex::start(
                std::sync::Arc::clone(engine)
                    as std::sync::Arc<dyn crate::record_index::RecordSource>,
                std::sync::Arc::new(crate::record_index::AppReader::of_device()),
                options.search.content.clone(),
            )
        });
        let record_changes = record_index.clone().map(|index| {
            index as std::sync::Arc<dyn tilo_action_broker::services::RecordIndexChanges>
        });
        let channels = engine.as_ref().and_then(|engine| {
            channels::Channels::open(
                engine,
                std::sync::Arc::clone(&channel_link),
                std::sync::Arc::clone(&channels_broker),
            )
            .map_err(|error| tracing::warn!(%error, "the channel service runs no threads"))
            .ok()
        });
        // The agent door toward the person's other devices: answered on this device's
        // endpoint, and served by the agent door once it opens.
        let remote = remote_door::Remote::new(options.wake.handle());
        let sync = start_collaboration(
            &options,
            continuity.clone(),
            engine.clone(),
            (beside, &remote),
            Some(sync::PeerDocuments::new(
                std::sync::Arc::clone(&channels_broker),
                std::sync::Arc::clone(&channels_fanout),
                record_changes.clone(),
            )),
        );
        // The content the workspaces' records reference: kept beside their
        // records under this session's state, and asked of the workspace's
        // other devices on the topic the records travel.
        let record_content = content::Plane::open(options.state.as_deref());
        if let Some(sync) = &sync {
            sync.attach_content(std::sync::Arc::clone(&record_content));
            // What this session says when another of the person's devices
            // offers it work: the inputs its own content already keeps.
            if let Some(actions) = sync.device_actions() {
                actions.attach_content(std::sync::Arc::clone(&record_content));
            }
        }
        // What other members send here is the workspaces' to admit: their
        // policies are read from the desk this session shows.
        if let Some(sync) = &sync {
            sync.attach_desk(handle.clone());
        }
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
        // The person's own record of what their devices answered when one of
        // them asked to be unlocked reads their fabric: what it carries is
        // written there as their history.
        if let Some(link) = &fabric {
            activity.watch_fabric(Arc::clone(link));
        }
        // Nearby Send: one engine for every file that leaves this device for
        // another of the person's, whether a screenshot the studio sends or a
        // file Files hands over. It stages into this machine's own spool, and
        // the session's pass turns when a hand-over moves.
        let nearby = match nearby_transfer::NearbyTransfer::open(
            fabric.clone(),
            nearby_transfer::spool_root(),
            continuity,
            options.wake.handle(),
        ) {
            Ok(nearby) => Some(nearby),
            Err(error) => {
                tracing::warn!(%error, "no thread takes what is sent nearby");
                None
            }
        };
        if let (Some(sync), Some(nearby)) = (&sync, &nearby) {
            sync.attach_nearby(std::sync::Arc::clone(nearby));
        }
        // The machine's status, which the bar's status cluster shows as the
        // battery, network and volume glyphs, the lock screen shows under its
        // card and the desk reads: every part of it is read off the services
        // that publish it.
        let readings = Rc::new(RefCell::new(status::Readings::default()));
        // Every app shows its live activity on this one board, which the
        // bar's pill reads and the floating layer draws as cards.
        let live_activities = std::sync::Arc::new(runtime_collab::LiveActivities::default());
        // A timer starting in the background must reach the bar on the next
        // frame, not the next minute.
        live_activities.set_host_wake(options.wake.handle());
        // The cards apps draw themselves, over the desk: the session
        // authorises what each presents, and the floating layer's place is
        // where the desk stands its window.
        let cards = crate::cards::Cards::new();
        desktop.with_policy(|policy| policy.use_cards(cards.placements()));
        let activities = Rc::new(chrome::Activities::new(
            std::sync::Arc::clone(&live_activities),
            cards.clone(),
        ));
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
        // Whether the person may invite someone into the workspace in front,
        // which the loop answers every pass: the bar offers its invite button
        // and the roster its footer from it.
        let invitable: Rc<Cell<bool>> = Rc::default();
        // The windows this session shares with its workspaces: a share's
        // encoder wakes the loop when it has a frame, and the bar reads them.
        // A window another device shares is decoded on the same waker, so a
        // picture that comes out late lands without waiting for the next
        // frame the desk draws for something else.
        let window_shares = window_share::WindowShares::new(
            options.wake.handle(),
            options.person.clone(),
            options.salt,
        );
        handle.set_peer_picture_wake(Some(options.wake.handle()));
        // The emoji picker is the chrome's; the session holds its picks,
        // writes them into the focused field and keeps its recents.
        let emoji = emoji::Emoji::mount();
        // Settings' Identify answers: the session reads the request each pass
        // and the bar says each display's number and name on it.
        let numbers = display_identify::Numbers::default();
        let published_displays = shared_displays::Publisher::default();
        // One table for the keys the session answers and the guide that
        // explains them: a chord that works is a chord the guide and the
        // menus show, and a person's rebinding reaches every one of them.
        let shortcuts = Rc::new(RefCell::new(std::mem::take(&mut options.config.shortcuts)));
        let chrome = Rc::new(RefCell::new(chrome::host(
            &handle,
            &options,
            chrome::ChromeWiring {
                intents: Rc::clone(&intents),
                asks: Rc::clone(&asks),
                shortcuts: Rc::clone(&shortcuts),
                feeds: chrome::BarFeeds {
                    signals: Rc::clone(&notice_signals),
                    readings: Rc::clone(&readings),
                    activities: Rc::clone(&activities),
                    notices: notices.clone(),
                    tray: tray.clone(),
                    fullscreen: Rc::clone(&fullscreen),
                    shares: Rc::clone(&window_shares),
                    reveals: Rc::clone(&reveals),
                    identify: numbers.ask(),
                    invitable: Rc::clone(&invitable),
                },
                engine: engine.clone(),
            },
            sync.clone(),
            &overview,
            &emoji.link(),
            &overlays,
            &switcher,
        )));
        compositor.add_host(shell::under(options.shell, Held(Rc::clone(&chrome))));
        // Search's code is asked for as the session starts, so it is in by the
        // time someone searches; a search asked for before then opens now,
        // and the desk publishes it on its next pass.
        handle.search_palette().borrow_mut().adopt();
        // The desk as the session publishes it, which the shell reads through the door.
        let chrome_door = shell::chrome_door(options.shell, &chrome, surfaces.clone());
        doors.add(Rc::clone(&chrome_door));
        // The seam the agent door's connections ask this session's own doors on: made before
        // the plane starts, which serves the programs.
        let session_doors = std::sync::Arc::new(agent_door::SessionDoors::new(
            options.wake.handle(),
            &options.person,
        ));
        let task_programs = Some(std::sync::Arc::new(task_programs::TaskPrograms::new(
            std::sync::Arc::clone(&session_doors),
        )));
        // The session's own actions, offered to a turn's agent as the broker's steps: the
        // arrangement, its undo, the windows and the notices, which the REPL already calls.
        // Each is answered on the loop's pass, where the arrangement is.
        let session_actions = std::sync::Arc::new(session_actions::SessionActions::new(
            std::sync::Arc::clone(&session_doors),
        ));
        session_doors.keep_actions(std::sync::Arc::clone(&session_actions));
        // The agents the person admits at its door, as the desk and the bar show them.
        let seats = agent_door::Seats::new();
        let desk = || desk_resource::Desk {
            seats: std::sync::Arc::clone(&seats),
            handle: handle.clone(),
            person: options.person.clone(),
            signals: Rc::clone(&notice_signals),
            activities: Rc::clone(&activities),
            surfaces: surfaces.clone(),
            sync: sync.clone(),
            notices: notices.clone(),
            tray: tray.clone(),
            engine: engine.clone(),
        };
        let desk_service = desk();
        let shell = if options.shell {
            Some(
                shell::Shell::mount(
                    &mut compositor,
                    &mut doors,
                    desk(),
                    wells,
                    presentation.theme.clone(),
                )
                .map_err(|why| {
                    Error::Backend(tilo_compositor::BackendError(format!(
                        "the shell did not start: {why}"
                    )))
                })?,
            )
        } else {
            None
        };
        // The surfaces the shell stands in for read as nothing, as they draw
        // nothing: the shell's parts are what a client finds there, while the
        // desktop's own desk and chrome keep their actions.
        let aside: Vec<tilo_surface_host::Shared> = if shell.is_some() {
            vec![Rc::clone(&desk_door), Rc::clone(&chrome_door)]
        } else {
            Vec::new()
        };
        // Who is here, over the windows: the roster the bar's faces open and
        // where everyone else's pointer is.
        let people = people::mount(
            &mut compositor,
            &mut doors,
            &options,
            &handle,
            sync.clone(),
            Rc::clone(&intents),
            Rc::clone(&invitable),
        );

        // What the floating layer keeps clear of: the banners' stack while a
        // banner shows, and the readout's band while it stands.
        let banner_stack = crate::panels::Reserved::new();
        let readout_band = crate::panels::Reserved::new();
        let _activity_layer = mount_activities(
            &mut compositor,
            &mut doors,
            &activities,
            &cards,
            banner_stack.clone(),
            readout_band.clone(),
            &options,
        );
        let banners = mount_banners(
            &mut compositor,
            &mut doors,
            &notices,
            &handle,
            &options,
            banner_stack,
        );

        // The sheets, over the desktop and its panels.
        let mount::sheets::Sheets {
            power,
            workspace_settings,
            portal,
            file_picker,
            directory,
            invite,
            delete_workspace,
            display_confirm,
            displays: display_seat,
            shortcut_guide: guide_seat,
            add_app,
            uninstall,
            lock_view,
        } = mount::sheets::mount_sheets(&mut compositor, &mut doors, &options);
        // Where a report about Tilo itself goes, as this computer is told:
        // the box a report is written in opens to it.
        let feedback = feedback::Destination::configured();
        // What only the session can do, offered the same way to every
        // surface that shows it: the workspace menu, the Tilo menu, search.
        host_actions::publish(
            &handle,
            mount::sheets::power_offers(&options),
            directory.is_some(),
            feedback.is_some(),
        );

        // The one worker that encodes a recording and writes every capture's
        // file, and what it makes of the recorder's work.
        let (capture_io, capture_notes) = capture_io::CaptureIo::new(options.wake.handle());
        // The capture studio, over everything it captures.
        let capture_studio = capture_studio::mount(
            &mut compositor,
            &mut doors,
            &options,
            nearby.as_ref(),
            Rc::clone(&capture_io),
            capture_notes,
            &live_activities,
        );

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
        let (commands, controls_reading, invocations, triggered, readout_place, readout) =
            mount_controls(
                &mut compositor,
                &mut doors,
                Rc::clone(&shortcuts),
                portal_shortcuts,
                guide_keys,
                KeyContext {
                    controller: Rc::clone(&controller),
                    controller_keys: Rc::clone(&controller_keys),
                    displays: display_seat
                        .as_ref()
                        .map(|seat| tilo_sheets::Seat::presentation(seat).clone()),
                    fold,
                    overlays: overlays.clone(),
                    desk: Rc::clone(&desk_overlays),
                    reveals: Rc::clone(&reveals),
                    panel: panel_keys.clone(),
                },
                options.scale,
                readout_band,
            );
        // The readout draws in the person's theme like the desk and the shell
        // do: its ground is the theme's floating surface, never a fixed fill.
        readout.borrow_mut().set_theme(presentation.theme);

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
        // A launch a service, an app or an agent asks the session for waits
        // for the pass that starts it: the same path the desk's own launches
        // take.
        options.launches.waking(options.wake.handle());

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
        // What each of this device's workspaces revokes, read from the
        // workspaces the engine holds: the broker's decisions and the
        // automation's arming both refuse a device its workspace revoked.
        let revocations = engine::revocations_of(engine.as_ref());
        // The typed opens an app asks this session for: one service, made
        // here so the broker below serves Files' waits for its lists'
        // changes, and started from the doors table.
        let tools = mount::tools::Handle::new(
            std::sync::Arc::clone(&options.launches),
            options.wake.handle(),
        );
        let action_plane = start_action_plane(
            &options,
            &live_activities,
            &handle,
            std::sync::Arc::new(shortcut_library),
            &activity,
            mount::services::Knitted {
                turns: Some(conversations.runs()),
                agents: Some(agents.roster()),
                tools: Some(tools.clone()),
            },
            mount::services::Collaboration {
                revocations: std::sync::Arc::clone(&revocations),
                sync: sync.as_deref(),
                engine: engine.as_ref().map(std::sync::Arc::clone),
                sealed: plane::Sealed {
                    broker: std::sync::Arc::clone(&channels_broker),
                    keys: engine.as_ref().map(|engine| engine.keys()),
                },
                content: Some(std::sync::Arc::clone(&record_content)),
                records: record_index,
                fanout: std::sync::Arc::clone(&channels_fanout),
                task_programs,
                session_actions: Some(std::sync::Arc::clone(&session_actions)),
                nearby: nearby.as_ref().map(std::sync::Arc::clone),
            },
        )?;
        // An action an app publishes, named by a search row, a link or a
        // person's card, is delivered through the broker: the app starts for
        // it when it is closed, and the action reaches it wherever it runs.
        if let Some(launch) = &options.launch {
            launch.lend_broker(action_plane.as_ref().and_then(Plane::agent_broker));
        }
        // What Files may read, and where a received file may land, is what
        // this session's workspaces grant it: read off the desk the broker
        // decides against.
        if let (Some(nearby), Some(plane)) = (&nearby, &action_plane) {
            nearby.attach_authority(std::sync::Arc::clone(&plane.desk)
                as std::sync::Arc<dyn tilo_action_broker::services::WorkspaceAuthority>);
        }
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
        // Every service this session serves, from the one table that says
        // who serves each: the identity an app signs in through, the
        // machine's readings an app the launcher connects reads, and the
        // data this device used. They start here, before any app does.
        let table = mount::doors::start(mount::doors::Serving {
            desk: action_plane
                .as_ref()
                .map(|plane| std::sync::Arc::clone(&plane.desk)),
            person: options.person.clone(),
            render_node: compositor.render_node(),
            keys: engine
                .as_ref()
                .map_or_else(Default::default, |engine| engine.keys()),
            questions: questions.clone(),
            consents: options
                .desktop
                .as_ref()
                .and_then(|desktop| desktop.consents())
                .cloned(),
            tools: Some(tools),
            wake: options.wake.handle(),
        });
        tilo_startup_metrics::mark("doors-done");
        // The control door `tilo-control` reaches: the development socket and the device admin's,
        // both answered over the broker this session's plane starts. It exists whether or not a
        // socket opens, because a program inside an app this session started is served at it too.
        // The activity undoes an agent's run in the records it changed through the same door.
        let door_host = std::sync::Arc::new(
            agent_door::Host::new(
                action_plane.as_ref().and_then(Plane::agent_broker),
                std::sync::Arc::clone(&session_doors),
            )
            .with_session_actions(Some(std::sync::Arc::clone(&session_actions))),
        );
        let undo_door: std::sync::Arc<dyn task_undo::UndoDoor> = door_host.clone();
        activity.attach_door(undo_door);
        let control = test_control::Control::open(&options, door_host);
        tilo_startup_metrics::mark("control-door-open");
        let door = agent_door::mount_named(
            &options,
            questions.clone(),
            std::sync::Arc::clone(&session_doors),
            control.door(),
            std::sync::Arc::clone(&seats),
        );
        // Beside the harness's control socket: whether the loop still turns,
        // and where every thread stands when it does not. A session without
        // one serves no such door, and no other session does either.
        let liveness = options
            .test_control
            .as_deref()
            .and_then(|control| compositor.serve_liveness(control, agent_door::bind_private));
        let agent_door = door.map(|door| {
            let mut door = door.serve(
                action_plane.as_ref().and_then(Plane::agent_broker),
                action_plane
                    .as_ref()
                    .map(|plane| std::sync::Arc::clone(&plane.desk)),
            );
            door.hear_remote(std::sync::Arc::clone(&remote));
            door
        });
        let automation = automation::Automation::new(
            options.state.as_deref(),
            options.wake.handle(),
            handle.clone(),
            options.person.clone(),
            saved_shortcuts,
            automation::Feeds {
                // Which of the person's devices this one is, and which of
                // them are online: each schedule runs on one of them.
                fabric: sync.as_ref().and_then(|sync| sync.fabric()),
                // The apps this session starts, the person's focus modes, and
                // what its workspaces revoke: each is a trigger a shortcut can answer.
                launches: std::sync::Arc::clone(&options.launches),
                policy: std::sync::Arc::clone(&notification_policy),
                revocations,
            },
        );

        // The shell-ui socket this session serves: what an app asks about
        // its own tabs arrives there and is answered on this loop's pass.
        let tabs = mount::app_tabs::AppTabs::start(
            std::sync::Arc::clone(&options.launches),
            options.wake.handle(),
            cards,
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
            options.wake.handle(),
        );
        // The lock, and the screen that shows it: the session keeps what the
        // screen shows and publishes it, so the screen is an app of the
        // session's like any other.
        let lock = lock_service::Lock::default();
        // The on-screen keyboard's state, published the same way: the app of
        // the session draws what the session says it does.
        let keyboard = keyboard::Keyboard::default();
        let mut started = std::mem::take(&mut options.services);
        started.push(Box::new(desk_service));
        // A service that records who changed what records the person's, not
        // the id its caller made it with: the session knows who sits here
        // before it starts them.
        for service in &mut started {
            service.seated(&options.person, &seat.earlier);
        }
        let services = mount_services(&mut doors, started);
        // A launch asks the services whether one runs the app's model, and
        // where one does names the resource it publishes, so the app launches
        // hosted and opens no record of its own
        // (`docs/architecture/apps.md` §8 step 2, `one-app-model.md` §4).
        if let Some(launch) = &options.launch {
            launch.lend_services(Rc::clone(&services));
        }
        // The session's own controls, served beside its services: an app
        // reads what they are now and invokes what changes them the way it
        // does every other resource of this device's.
        services
            .borrow_mut()
            .add(Box::new(controls::SessionControls::new(
                Rc::clone(&commands),
                Rc::clone(&controls_reading),
            )));
        // Chat's conversations as a service, so a place that shows one draws Chat's own channel
        // page from its reading: the canvas's ask places `chat.workspace/channels/<id>`, and the
        // shell reads that reading here (one-app-model.md §2, shell.md §8.1).
        services
            .borrow_mut()
            .add(Box::new(conversations::ChatViewService::new(Rc::clone(
                &conversations,
            ))));
        // The shell's own records reach the door as any app's do the moment its package runs.
        if let Some(shell) = &shell {
            shell.share_records(&services);
        }
        // The shell's own readings and verbs, on the same door every other
        // service answers on.
        mount::shell::Shell::mount(&services, &handle, overlays.clone(), Rc::clone(&shortcuts));
        services
            .borrow_mut()
            .add(Box::new(lock_service::LockService::new(lock.clone())));
        // A window shared with a workspace, and the windows its members share.
        services
            .borrow_mut()
            .add(Box::new(window_share::WindowShareService::new(Rc::clone(
                &window_shares,
            ))));
        // The displays, on the same door: their arrangements land as the
        // commands the loop carries out, their brightness is set at once, and
        // the reading says where each display stands.
        let displays_service =
            displays::Displays::mount(&services, options.backlights.take(), Rc::clone(&commands));
        // A brightness key or slider anywhere wakes the loop, so a display
        // dimmed in software follows at once.
        displays_service.watch(&options.wake.handle());
        services
            .borrow_mut()
            .add(Box::new(keyboard::KeyboardService::new(keyboard.clone())));
        // Dictation joins the services: its recognizer, microphone and
        // consent are only the session's, and the sheet asks about them.
        let dictation =
            dictation::Dictation::mount(&mut compositor, &mut doors, &services, &options);
        // The questions about the person's apps answer through the registry
        // of their downloaded programs, which is one of the services.
        let app_sheets =
            app_sheets::AppSheets::new(add_app, uninstall, Rc::clone(&services), notices.clone());

        let search = Rc::new(SessionSearch::new(
            options.search,
            options.person.clone(),
            options.apps,
            Some(options.wake.handle()),
        ));
        session_search.get_or_init(|| Rc::clone(&search));
        let asker = questions::Asker::new(
            questions,
            portal.clone(),
            file_picker.clone(),
            Rc::clone(&services),
        );
        let workspace_tabs =
            workspace_settings::WorkspaceTabs::new(cli::data_home(), options.wake.handle());
        let directory_store = directory::Store::default();
        let boot_join = boot_join::BootJoin::at_boot(&directory_store, &handle);
        let community = community::Community::for_site(directory::community_site());
        let mut shared = Shared {
            handle,
            desktop,
            activity,
            person: options.person.clone(),
            earlier: seat.earlier,
            search,
            launch: options.launch,
            restore: RefCell::new(restore),
            drives: options.drives,
            asks,
            send_from: Cell::default(),
            report_from: Cell::default(),
            feedback,
            intents,
            quick_settings: quick_settings::installed(&options.surfaces),
            panel_asks: Rc::clone(&panel_asks),
            panel_back: Cell::new(None),
            overlays,
            surfaces: one_surface,
            desk_overlays,
            switcher,
            doors,
            invocations,
            triggered,
            commands,
            controls: Rc::clone(&controls_reading),
            waiting: RefCell::default(),
            readings,
            services,
            table,
            window_shares,
            seats,
            hosted: !options.machine.is_owned(),
            fabric: fabric.clone(),
            vpn,
            devices: mount::devices::Devices::default(),
            bluetooth: bluetooth_handoff::Handoffs::default(),
            handoff: Rc::new(std::cell::RefCell::new(None)),
            waking: mount::wake::Wake::new(&options.wake_seams),
            wake_handle: options.wake.handle(),
            music,
            power,
            workspace_settings,
            ws_settings_fed: Cell::new(None),
            capabilities_fed: Cell::new(None),
            fs_authority: fs_authority::Publisher::for_session(),
            workspace_tabs,
            conversations,
            agents,
            directory,
            invite,
            invite_made: RefCell::default(),
            delete_workspace,
            delete_workspace_ask: RefCell::default(),
            directory_store,
            boot_join: RefCell::new(boot_join),
            community: RefCell::new(community),
            portal,
            file_picker,
            lock,
            keyboard,
            owner_session: owner_session::SessionFollow::default(),
            display_confirm,
            notices: notices.clone(),
            moves: window_move::Moves::new(notices.clone(), std::sync::Arc::clone(&notes)),
            activities,
            tray,
            wallpapers,
            lock_layouts: options.config.keyboard_layouts.clone(),
            lock_layout: Cell::new(0),
            session_actions: options.session_actions,
            observed: RefCell::new(None),
            here_at: Cell::new(0),
            here_seen: Cell::new(0),
            places_seen: RefCell::default(),
            notifications: {
                let notifications = notifications::SessionNotifications::new(
                    options
                        .desktop
                        .as_ref()
                        .map(|desktop| desktop.notifications().clone()),
                    notices,
                    banners.clone(),
                    notification_policy,
                    notice_signals,
                    options.power.clone(),
                    notice_history,
                );
                // Notifications continue on the person's other devices, when
                // this session runs as one of them.
                if let Some(link) = &fabric {
                    notifications.attach_fabric(std::sync::Arc::clone(link));
                }
                Rc::new(RefCell::new(notifications))
            },
            asker,
            agent_door: RefCell::new(agent_door),
            session_doors,
            remote: std::sync::Arc::clone(&remote),
            folder_import: RefCell::new(None),
            sync,
            engine,
            desktops: shared_desktop::SharedDesktops::new(std::sync::Arc::clone(&notes)),
            notes,
            membership_fed: Cell::new(None),
            _channels: channels,
            personal,
            clipboard: Rc::new(mount::clipboard::Clipboard::new(options.wake.handle())),
            arrangement,
            underway: busy,
            settings: mount::settings_sync::SettingsSync::new(),
            fleet_renewal: mount::renewal::FleetRenewal::for_joined_device(fabric)
                .map(std::sync::Arc::new),
            overview,
            dictation,
            emoji,
            shortcut_guide,
            chrome,
            people,
            fullscreen,
            invitable,
            hide_top_bar,
            controller,
            controller_keys,
            app_sheets,
        };
        // A package the person opens authors its own records in the workspace
        // it opens in, as the person there: the workspace's governance gives
        // it before the process is planned, so the app starts able to keep
        // what they do in it.
        if let Some(launch) = &shared.launch {
            let handle = shared.handle.clone();
            let engine = shared.engine.clone();
            launch.lend_authorship(Rc::new(move |workspace: &str, app: &str| {
                if let Err(error) =
                    crate::membership::authorship(engine.as_deref(), &handle, workspace, app)
                {
                    tracing::warn!(
                        %workspace,
                        app,
                        %error,
                        "an opening app was not let author its workspace"
                    );
                }
            }));
        }
        shared.start_search();
        // What the person copied is personal: the clipboard publishes the
        // copies itself, as a reading only a caller the workspace grants reads.
        shared
            .services
            .borrow_mut()
            .add(Box::new(mount::clipboard::Served::new(Rc::clone(
                &shared.clipboard,
            ))));
        // What people said to this person is personal too: the centre
        // publishes its rows itself, as a reading only a granted caller
        // reads, and the dismiss and act verbs the panel invokes.
        shared
            .services
            .borrow_mut()
            .add(Box::new(notifications::Served::new(Rc::clone(
                &shared.notifications,
            ))));
        // The session's own diagnostics: the profiler and the heap trim, which
        // the device admin and a person's own scripts read and drive through
        // the same door as the desktop's other actions.
        shared
            .doors
            .add(Rc::new(std::cell::RefCell::new(diagnostics::Served)));
        // The person's devices are what their other machines say of
        // themselves, which is nobody's personal content: their reading is
        // published for any app that may read the session's resources.
        if let Some(fabric) = shared.fabric.clone() {
            shared
                .services
                .borrow_mut()
                .add(Box::new(devices::Served::new(
                    fabric,
                    Rc::clone(&shared.readings),
                    Rc::clone(&shared.devices.chosen),
                    Rc::clone(&shared.handoff),
                    Rc::clone(&shared.devices.pressed),
                    Rc::clone(&shared.devices.handed),
                )));
        }
        // The broker decides a granted reading when the app asks, which is
        // before this loop's first pass: what the services declare is read
        // here, with every service mounted, so a connection that arrives as
        // the session starts meets the workspace's grants and not an empty
        // declaration.
        if let Some(plane) = &action_plane {
            plane.link.declare(&shared.services.borrow());
        }
        // What the portal brings comes from any thread, and wakes the loop
        // instead of waiting for its next turn.
        if let Some(portal) = &options.portal {
            compositor.set_input_observer(Some(portal.input_observer()));
        }
        let loop_wake = compositor.waker();
        // Signing in does not change the login screens; what the person
        // changes here afterwards does, where they let it follow.
        let comforts = options.machine.is_owned().then(|| {
            let wake = loop_wake.clone();
            mount::comforts::Mirror::mount(std::path::PathBuf::from(tilo_auth::SOCKET), move || {
                wake.wake_by_ref();
            })
        });
        let surface_apps = std::mem::take(&mut options.surfaces);
        let slot_views = slot_views::SlotViews::new(
            shared.handle.clone(),
            shared.overlays.clone(),
            options.scale,
        );
        // Assistive technology reads the desktop through the same endpoint an
        // app's window has: what a client asks wakes this loop, which is
        // where every tree and every action lives.
        let wake = loop_wake.clone();
        let a11y = a11y::Accessibility::new(Arc::new(move || wake.wake_by_ref()), aside);
        // A session left locked by one that did not end in order comes back
        // locked, as the person left it.
        let starts_locked = options
            .recovery
            .as_ref()
            .is_some_and(recovery::Recovery::starts_locked);
        let mut session = Self {
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
            lock_starts: 0,
            lock_view: lock_view.map(lock_screen::LockView::new),
            lock_shown: lock_screen::LockShown::App,
            recovery: options.recovery.take(),
            update_finish: update_finish::Finish::for_session(options.desktop.as_deref()),
            captures: options.captures,
            capture_io,
            capture_studio,
            windows: window_motion::WindowMotions::new((options.presentation)().theme.motion()),
            quick_settings_panel: quick_settings::PanelMotion::new(
                (options.presentation)().theme.motion(),
            ),
            warmth,
            language: mount::language::Language::new(),
            night_light,
            auto_brightness,
            keyboard_preference: options.config.on_screen_keyboard,
            keyboard_run: keyboard::Run::default(),
            keyboard_starts: 0,
            quick_settings_run: quick_settings::Run::Down,
            quick_settings_starts: 0,
            portal: options.portal,
            launches: std::sync::Arc::clone(&options.launches),
            action_plane,
            content: record_content,
            control,
            liveness,
            automation,
            automation_runtime,
            screen_locked: false,
            desktop: options.desktop,
            running: Vec::new(),
            pids: std::collections::BTreeMap::new(),
            window_artwork: std::collections::BTreeMap::new(),
            _settings_watch: settings_watch,
            surfaces,
            surface_apps,
            slots: mount::slots::Slots::default(),
            slot_views,
            displays: display_settings::Displays::default(),
            displays_service,
            readout_place,
            readout,
            numbers,
            published_displays,
            confirmation: display_confirm::Confirmation::default(),
            display_sheet: display_sheet::DisplaySheet::new(
                display_seat,
                options.projector.clone(),
                &options.wake.handle(),
            ),
            projection_video: projection_video::ProjectionVideo::new(),
            rotation: rotation::Rotation::mount(options.accelerometer),
            sources,
            input_method: input_method::InputMethod::default(),
            shell,
            a11y,
            ended: false,
            first_frame: false,
            active_workspace: None,
            published_steps: Some(std::sync::Arc::clone(&session_actions)),
            kept_at: Instant::now(),
            tool_input: std::time::Duration::ZERO,
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
            connectors: mount::connectors::ConnectorHost::new(),
        };
        session.watch_sockets()?;
        session.shared.directory_store.waking(options.wake.handle());
        mount::connectors::start_all(&session);
        if starts_locked {
            session.compositor.lock_now()?;
        }
        tilo_startup_metrics::mark("session-build-done");
        Ok(session)
    }

    /// Make every socket this session serves end its loop's wait: an app that
    /// connects, an importer that asks, a program that drives the desktop is
    /// answered on the pass that follows, not on a tick of the loop.
    ///
    /// # Errors
    /// The loop took no more sources.
    fn watch_sockets(&mut self) -> Result<(), Error> {
        if let Some(tabs) = &mut self.tabs {
            tabs.wake_on(&mut self.compositor)?;
        }
        if let Some(door) = self.shared.agent_door.get_mut() {
            door.wake_on(&mut self.compositor)?;
        }
        self.control.wake_on(&mut self.compositor)?;
        if let Some(import) = self.shared.folder_import.get_mut().as_mut() {
            import.wake_on(&mut self.compositor)?;
        }
        Ok(())
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
        // The desktop this session runs inside hands it over as one
        // connection the window takes once, so the window opens before the
        // session builds anything of its own: the desk builds the surface
        // apps' renderers, whose adapter request connects to that desktop,
        // and after that the window would find no connection left.
        let window = tilo_compositor::NestedBackend::open_window(config)?;
        let mut session = Self::new(options, |policy| Compositor::nested(policy, config, window))?;
        session.warm()?;
        Ok(session)
    }

    /// Warm the displays when the person left the night light on, and start
    /// the surface apps: their windows wait for the outputs the session has
    /// now added.
    fn warm(&mut self) -> Result<(), Error> {
        self.follow_displays()?;
        self.follow_fullscreen();
        self.shared.follow_invitable();
        for surface in &self.surface_apps {
            // The lock screen is started when the session locks, the
            // on-screen keyboard when the person may need it and quick
            // settings while its slot is up: what their windows show is never
            // the desk's to open.
            if surface.lock
                || surface.name == keyboard::SURFACE
                || surface.name == quick_settings::SURFACE
            {
                continue;
            }
            let workspace = self.shared.active_workspace_id();
            self.surfaces
                .start_or_log(&mut self.compositor, surface, &workspace);
        }
        Ok(())
    }

    /// Bring the displays in line with the person's arrangement and the way up
    /// the device is held, and advance a change waiting for their answer.
    fn follow_displays(&mut self) -> Result<(), Error> {
        // The panel follows the light in the room while the person asked for
        // it, and the displays are warmed to what the night light shows now;
        // a new output takes both with the displays' arrangement.
        let night_light = self.follow_light()?;
        let turned = self.rotation.follow(&self.compositor.outputs());
        // What the displays service publishes follows what the compositor
        // shows: which ones there are, and which one the person works on.
        self.displays_service
            .land(&self.shared.handle, &self.compositor.outputs());
        // A display with neither a backlight nor DDC/CI keeps its brightness
        // control: the compositor darkens what it shows to it.
        self.displays_service.dim(&mut self.compositor)?;
        // Settings asked which screen is which: each display says its number
        // and name for a moment.
        self.numbers.follow(
            self.compositor.display_snapshot(),
            &self.compositor.outputs(),
        );
        // Where this device's screens stand reaches the person's other
        // devices, and what every device shows is published for the shared
        // arrangement the display sheet and Settings show.
        if let Some(fabric) = &self.shared.fabric {
            self.published_displays
                .follow(fabric, self.compositor.display_snapshot());
        }
        self.displays
            .follow(&mut self.compositor, night_light, turned)
    }

    /// Keep the readout's facts current: where the displays stand, where the
    /// person types, and a mode change waiting for the readout to confirm it.
    fn follow_readout(&self) {
        let caret = self.compositor.text_caret().map(|caret| {
            ui::kurbo::Rect::new(
                caret.x,
                caret.y,
                caret.x + caret.width,
                caret.y + caret.height,
            )
        });
        self.readout_place
            .borrow_mut()
            .observe(&self.compositor.outputs(), caret);
        if let Some(on) = self.shared.notifications.borrow().take_quiet_change() {
            self.show_hint(Hint {
                kind: OsdKind::DoNotDisturb,
                on,
                chord: None,
                name: None,
            });
        }
    }

    /// Place the surfaces over the desk on the displays as they are now: the
    /// outputs' rectangles, and the one holding the pointer, which is the
    /// display the person works on.
    fn follow_overlays(&self) {
        let outputs: Vec<ui::kurbo::Rect> = self
            .compositor
            .outputs()
            .iter()
            .filter(|output| output.enabled)
            .filter_map(|output| output.rect)
            .map(|(x, y, w, h)| {
                ui::kurbo::Rect::new(
                    f64::from(x),
                    f64::from(y),
                    f64::from(x) + f64::from(w),
                    f64::from(y) + f64::from(h),
                )
            })
            .collect();
        let (x, y) = self.compositor.pointer_location();
        let focused = outputs
            .iter()
            .copied()
            .find(|output| output.contains(ui::kurbo::Point::new(x, y)));
        self.shared.overlays.set_displays(&outputs, focused);
        // Where the person types, for the surfaces a key opens at the caret.
        self.shared
            .overlays
            .set_caret(self.readout_place.borrow().caret());
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
        let workspace = self.shared.active_workspace_id();
        self.surfaces
            .start(&mut self.compositor, surface, &workspace)
    }

    /// Stop the surface app `name`, if it runs.
    pub fn stop_surface(&mut self, name: &str) {
        self.surfaces.stop(name);
    }

    /// The surface the session starts as its lock screen, when it has one.
    #[must_use]
    pub fn lock_screen(&self) -> Option<&surfaces::Surface> {
        self.surface_apps.iter().find(|surface| surface.lock)
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
        argument: InvokeArgument,
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
        self.shared.activities.board()
    }

    /// The media service the apps reach through the session's plane, while
    /// it serves one: what a call joins through.
    #[cfg(test)]
    pub(crate) fn media_service(&self) -> Option<&tilo_native_media::NativeMediaService> {
        self.action_plane.as_ref()?.calls_service.as_ref()
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
    pub fn remote_door(
        &self,
        devices: std::sync::Arc<dyn tilo_runtime::remote_door::PersonsDevices>,
    ) -> tilo_runtime::remote_door::RemoteDoor {
        self.shared.remote.door(devices)
    }

    /// The windows the overview pictures as the compositor draws them now: each window's id and
    /// the rect its picture rests at, logical `(x, y, width, height)` , bottom first. Empty while
    /// the overview is away, and before a window on the desk has a picture.
    #[must_use]
    pub fn overview_pictures(&self) -> Vec<(u64, (f64, f64, f64, f64))> {
        match self.shell.as_ref() {
            Some(shell) => shell.pictures(),
            None => self.shared.overview_pictures(),
        }
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

    /// The person's arrangement: the desk this session holds, with its history and undo.
    #[must_use]
    pub fn arrangement(&self) -> &Rc<RefCell<arrangement::Arrangement>> {
        &self.shared.arrangement
    }

    /// The broker this session's apps and agents reach its actions through, while it serves
    /// one: what a test asks for the catalogue an agent working a turn is offered, and invokes
    /// a step of it through, as the agent door does.
    #[cfg(test)]
    #[must_use]
    pub fn action_broker(
        &self,
    ) -> Option<std::sync::Arc<tilo_action_broker::collab_app_broker::AppBrokerState>> {
        self.action_plane.as_ref().and_then(Plane::agent_broker)
    }

    /// Chat's record of the workspace `workspace` as this session holds it: the
    /// conversation a person asked in, and its messages. `None` where the
    /// session holds no record of it yet. A test reads what a person's ask wrote
    /// through the same open record the session holds, never by opening a second
    /// writer on the same store.
    #[must_use]
    pub fn chat_of(&self, workspace: &str) -> Option<chat::Chat> {
        self.shared.conversations.chat(workspace)
    }

    /// What the windows ask of the workspace's activity and follow, as this
    /// session serves it: the same door the desk's own pane reads through.
    #[must_use]
    pub fn conversation_runs(
        &self,
    ) -> std::sync::Arc<dyn tilo_action_broker::services::TrackerRuns> {
        self.shared.conversations.runs()
    }

    /// What the windows ask of the workspace's activity and follow, as this session serves it: the
    /// same door the desk's own pane reads through.
    #[must_use]
    pub fn activity(&self) -> tilo_desktop_surface::session::ActivityRequests {
        self.shared.activity.requests()
    }

    /// The resource the text `text` typed on the canvas places: the top
    /// result of the one search the palette, the canvas and an agent's
    /// `find` run, as [`arrangement::found`] reads it. The same call
    /// `session.place_found` makes, for a caller that asks what a text names
    /// without placing it.
    #[must_use]
    pub fn placed_by(&self, text: &str) -> Option<String> {
        let search = Rc::clone(&self.shared.search);
        let catalog = self.shared.arrangement.borrow().targets();
        arrangement::found(text, &|needle| search.find(needle), |resource| {
            catalog.borrow().get(resource).is_some()
        })
    }

    /// Undo the person's last arranging step, through the published action
    /// as any caller does.
    ///
    /// # Errors
    /// The door's refusal: there is no step to take back.
    pub fn undo_arrangement(&mut self) -> Result<(), DoorError> {
        let address = ActionAddress::new(arrangement::RECORD_RESOURCE, "undo");
        self.shared.doors.invoke(
            &address,
            &serde_json::Map::new(),
            &mut crate::look::look(&mut self.compositor, Some(&self.content)),
        )?;
        Ok(())
    }

    /// Invoke a published action as `actor` (an agent, a script), beside
    /// [`Self::invoke`].
    ///
    /// # Errors
    /// The door's refusal: no such action, bad arguments, or one the caller
    /// may not run.
    pub fn invoke_with_actor(
        &mut self,
        actor: &str,
        resource: &str,
        action: &str,
        arguments: &serde_json::Value,
    ) -> Result<ResourceOperationStart, DoorError> {
        let empty = serde_json::Map::new();
        let arguments = arguments.as_object().unwrap_or(&empty);
        if let Ok(mut arrangement) = self.shared.arrangement.try_borrow_mut() {
            let _ = &mut arrangement;
        }
        self.shared.doors.invoke_as(
            actor,
            &ActionAddress::new(resource, action),
            arguments,
            &mut crate::look::look(&mut self.compositor, Some(&self.content)),
        )
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
        let started = self.shared.doors.invoke(
            &address,
            arguments,
            &mut crate::look::look(&mut self.compositor, Some(&self.content)),
        )?;
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

    /// The other people whose pointers the desk drew on this session, as the
    /// last frame drew them: their name and the point, in the surface's
    /// coordinates. Drawn over the window each is on, whatever the roster
    /// shows.
    #[must_use]
    pub fn drawn_peer_pointers(&self) -> Vec<(String, (f64, f64))> {
        self.shared.people.borrow().drawn_pointers()
    }

    /// What an assistive client reads of the session's own surfaces now: the
    /// place each surface publishes at, its window name and the tree behind
    /// it, as the AT-SPI exporter hands them over. What a test drives is the
    /// export a screen reader gets, not a parallel description of it.
    #[must_use]
    pub fn a11y_windows(&self) -> Vec<(usize, String, ui::accesskit::TreeUpdate)> {
        self.shared
            .doors
            .offering()
            .into_iter()
            .filter_map(|(place, _)| {
                let (name, tree) = a11y::Accessibility::window_at(&self.shared.doors, place)?;
                Some((place, name, tree))
            })
            .collect()
    }

    /// Perform `action` on `target` of the surface whose window is the
    /// `place`-th the exporter publishes: what a client does over AT-SPI.
    /// Answers whether that surface took the action.
    #[must_use]
    pub fn a11y_perform(
        &mut self,
        place: usize,
        target: ui::accesskit::NodeId,
        action: &ui::a11y::Action,
    ) -> bool {
        a11y::Accessibility::perform(&self.shared.doors, place, target, action)
    }

    /// Whether the person logged out, which ends the session's loop.
    #[must_use]
    pub const fn ended(&self) -> bool {
        self.ended
    }

    /// What stands before the desk runs its own pass: the wait for the clock
    /// finishes itself and gives the layer to what a verified identity opens.
    fn follow_setup(&self) {
        if let Some(setup) = &self.setup {
            setup.borrow_mut().step();
        }
    }

    /// Whether setup is up: it withholds the desktop until it is done.
    #[must_use]
    pub fn setup_up(&self) -> bool {
        self.setup.as_ref().is_some_and(|setup| setup.borrow().up())
    }

    /// Serve the installed folder importer for this session's `display`, the
    /// Wayland socket it bound: whether first use is over, and the review the
    /// person left to be opened once it is.
    ///
    /// Add folders reads the answer before it continues a saved import, so
    /// nothing it carries lands under an identity ceremony still under way.
    ///
    /// # Errors
    /// There is no runtime directory, or the socket cannot be bound.
    pub fn serve_folder_import(&mut self, display: &std::ffi::OsStr) -> std::io::Result<()> {
        let mut continuation = import_readiness::Continuation::serve(display)?;
        // The importer asks as the person clicks Add folders: its socket ends
        // the loop's wait, so the answer is drawn at once.
        continuation.wake_on(&mut self.compositor)?;
        *self.shared.folder_import.borrow_mut() = Some(continuation);
        Ok(())
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
        self.shared.overlays.is_open(Slot::QuickSettings)
    }

    /// The input preferences the compositor runs with now.
    #[must_use]
    pub const fn input_config(&self) -> &tilo_compositor::InputConfig {
        &self.input
    }

    /// Let each round of asks play out: a round can ask for another (a key's
    /// shortcut the chrome leaves to the session); a few rounds settle any
    /// chain.
    fn settle_asks(&mut self) -> Result<(), Error> {
        // Who may invite whom into the workspace in front is the workspace's
        // answer, and moves while the session runs: the bar's invite button
        // and the roster's footer follow it.
        self.shared.follow_invitable();
        for _ in 0..4 {
            self.shared.follow_asks(&mut crate::look::look(
                &mut self.compositor,
                Some(&self.content),
            ));
            // What was plugged in since, and what the person asked of its
            // notice: an open starts Files like any other launch.
            if let Some(drives) = &self.shared.drives {
                drives.follow(self.shared.launch.as_deref());
            }
            // What the desktop asked to open, and what stopped since.
            if let Some(launch) = &self.shared.launch {
                launch.follow();
            }
            // What the tools service has to know of this pass — the media
            // attached and the person's apps — and the sheets it left for the
            // loop, which only a pass can put up.
            if let Some(tools) = self.shared.table.tools() {
                let media = self
                    .shared
                    .drives
                    .as_ref()
                    .map(|drives| drives.live())
                    .unwrap_or_default();
                self.shared.follow_tools(tools, media);
            }
            // The windows this device left open, one at a time, each into the
            // tab it was in.
            self.shared.follow_restore();
            // What the apps asked of their own tabs, and of what a capture
            // may show of them, as the desk commands they are.
            self.follow_app_asks();
            self.shared.follow_gestures();
            self.shared.follow_tab_menus();
            self.follow_window_moves();
            let locked = self.compositor.locked();
            let ran = {
                let mut look = crate::look::look(&mut self.compositor, Some(&self.content));
                self.shared.run_invocations(locked, &mut look)
            };
            let commands = std::mem::take(&mut *self.shared.commands.borrow_mut());
            let commanded = !commands.is_empty();
            for command in commands {
                self.command(command)?;
            }
            if !ran
                && !commanded
                && self.shared.asks.borrow().is_empty()
                && !self.launches.waiting()
            {
                break;
            }
            // What ran may have queued desktop commands on the shared
            // handle (the chrome switching workspace): the desk carries
            // them out on its next pass.
            self.shared.desktop.with_widgets(WidgetHost::relayout);
        }
        Ok(())
    }

    /// Answer what the apps asked of their own tabs, and of what a capture
    /// may show of them, as the desk commands they are.
    fn follow_app_asks(&mut self) {
        if let Some(tabs) = &mut self.tabs {
            tabs.follow(
                &self.shared,
                &self.shared.desktop,
                self.shared.launch.as_deref(),
                &mut self.compositor,
                self.desktop.as_deref(),
                self.capture_studio.as_ref(),
            );
        }
    }

    /// Note what each window this session runs shows, as the broker knows the
    /// record its app holds: read every pass, so a window that opens a record,
    /// moves in it or closes is named by what it shows now.
    fn read_window_notes(&self) {
        let Some(plane) = &self.action_plane else {
            return;
        };
        let Some(broker) = plane.action_broker() else {
            return;
        };
        self.shared.notes.read(&broker.state().window_records());
    }

    /// Hand every agent following the catalogue the session's own actions again when what it
    /// offers moved: a place made, a place closed, a window opened, a notice put up. The rows
    /// are the same the REPL lists, so an agent arranging the screen sees what the person's own
    /// door publishes (shell.md §8.1, "Any action").
    fn follow_session_actions(&self) {
        let Some(actions) = &self.published_steps else {
            return;
        };
        // The rows read here, on the loop that holds the arrangement: nothing off this thread
        // reads a place. What is offered is compared with what was offered before — a live
        // surface's revision moves every pass, and a catalogue rebuilt for it would cost the
        // loop every frame — so the broker is told only when the rows themselves moved.
        let rows = tilo_actions::door::survey_placed(&self.shared.doors.resources());
        if actions.offer(rows)
            && let Some(broker) = self.action_plane.as_ref().and_then(Plane::agent_broker)
        {
            broker.host_steps_changed();
        }
    }

    /// Keep the workspaces this session takes part in current with the
    /// desktop's, and hand the desktop who is here.
    fn follow_collaboration(&self) {
        let Some(sync) = &self.shared.sync else {
            return;
        };
        self.shared.handle.read(|tree| sync.follow_desktop(tree));
        sync.follow();
        // A workspace's strips and windows are the workspace's: what its
        // members put there — before this device joined, and since — is this
        // device's desk too, and what this device arranges there is theirs to
        // see.
        self.shared.follow_shared_desktops();
        // This device's call, as everyone in the workspace reads it, and
        // theirs as this session's apps show them: a join or leave reaches a
        // channel's roster on the pass it happened on.
        if let Some(plane) = &self.action_plane {
            plane.follow_calls(sync);
        }
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
        // The chords the armed shortcuts answer are the table's own chords
        // from here on: the keys claim them for the automation host instead
        // of the desktop, and the guide shows what each runs.
        self.shared
            .shortcut_guide
            .follow_trigger_chords(self.automation.trigger_chords());
        // A chord nothing else answered reaches the automation host, which
        // starts every shortcut whose keyboard trigger names it.
        let pressed: Vec<settings_store::hardware::KeyChord> =
            std::mem::take(&mut *self.shared.triggered.borrow_mut());
        for chord in pressed {
            self.automation.press(&chord);
        }
    }

    /// The shortcut runs that finished and no history recorder took yet,
    /// oldest first: what the history names its changes by.
    #[must_use]
    pub fn take_finished_shortcut_runs(&self) -> Vec<FinishedShortcutRun> {
        self.automation.take_finished()
    }

    /// Ask the system to suspend, restart or shut the machine down.
    fn session_action(&self, action: SessionAction) {
        self.shared.session_action(action);
    }

    /// Lock the session as the person asked: with a key, a command, or
    /// because one of their other devices locked. A session that could not be
    /// left again never locks ([`Session::can_lock`]). The desk goes the
    /// moment the lock is asked for and not when the screen has started, so
    /// nothing of what was on it stays up for the start of a process; the
    /// screen that shows the lock is started here, and asks the compositor for
    /// the lock itself.
    pub(crate) fn lock_by_person(&mut self) -> Result<(), Error> {
        if !self.can_lock() {
            return Ok(());
        }
        self.compositor.lock_now()?;
        self.start_lock_screen();
        Ok(())
    }

    /// Whether this session can lock at all: a password it can check, and a
    /// screen that shows the lock and takes that password. Missing either, a
    /// lock would leave the session with nothing that could lift it, so it
    /// stays unlocked and says which part is missing.
    pub(crate) fn can_lock(&self) -> bool {
        if !self.unlocking.verifier.available() {
            tracing::warn!("no password can be checked here: not locking");
            return false;
        }
        // A session whose lock screen's app is not there still locks: the
        // session draws the lock itself.
        if self.lock_screen().is_none() && self.lock_view.is_none() {
            tracing::warn!("nothing here shows the lock: not locking");
            return false;
        }
        true
    }

    /// Paste what was copied into `destination`, beside what is already there.
    pub(crate) fn paste_files(&self, destination: &str) {
        self.shared.clipboard.paste_files(
            std::path::Path::new(destination),
            crate::mount::clipboard_files::Conflict::KeepBoth,
        );
    }

    /// Switch the machine to virtual terminal `vt`, as `Ctrl+Alt+F1..F12`
    /// asks. A desktop cannot give the machine back any other way: the
    /// terminal is where the person logs in, ends a stuck program or starts
    /// Tilo again. A backend without virtual terminals has nothing to switch,
    /// and the session stays.
    #[expect(
        clippy::unnecessary_wraps,
        reason = "every arm of the command match answers with one result"
    )]
    fn switch_vt(&mut self, vt: i32) -> Result<(), Error> {
        if let Err(error) = self.compositor.change_vt(vt) {
            tracing::info!(%error, vt, "the virtual terminal was not switched");
        }
        Ok(())
    }

    /// Show a mode change in the readout, said in words: a switch flipped by
    /// a key or a switch has nothing to read as a level.
    pub(crate) fn hint(&self, kind: OsdKind, on: bool) {
        self.show_hint(Hint {
            kind,
            on,
            chord: None,
            name: None,
        });
    }

    /// Show `hint` in the readout.
    pub(crate) fn show_hint(&self, hint: Hint) {
        self.shared.show_readout(&Osd::from(hint));
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // A profile still recording is completed, its files written, before
        // the process goes.
        tilo_frame_metrics::profile::shutdown();
        // The desk the person left, written whatever ended the session.
        self.shared.keep_desk_now();
        // Its apps' display goes with it, and so do they: the apps outlive
        // no session, whoever else still holds what started them.
        if let Some(launch) = &self.shared.launch {
            launch.end_running();
        }
    }
}
