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
mod app_icons;
mod app_names;
mod app_sheets;
pub mod arrangement;
mod asks;
mod automation;
mod bars;
mod call_devices;
mod calls;
pub mod capture;
mod capture_send;
mod capture_studio;
mod channels;
mod chrome;
mod clock_wait;
mod config;
mod content;
pub mod controller_nav;
pub mod controllers;
mod controls;
pub mod conversation;
mod desk;
mod desk_doc;
mod desk_migration;
mod desk_resource;
mod desk_restore;
pub mod desktop_services;
mod device_actions;
mod devices;
mod dictation;
mod directory;
mod display_confirm;
mod display_control;
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
mod import_readiness;
mod input_method;
mod input_sources;
pub mod keyboard;
mod keys;
mod launches;
mod lists;
pub mod live;
mod lock_screen;
mod lock_service;
mod membership;
pub mod messages;
pub mod mount;
pub mod notices;
mod notifications;
mod options;
pub mod overlays;
mod owner_session;
mod panels;
mod people;
mod plane;
pub mod portal_link;
mod press_outside;
mod projection;
mod projection_video;
mod questions;
pub mod quick_settings;
mod records;
mod rotation;
pub mod run;
mod run_programs;
mod seams;
mod search;
mod search_ask;
pub mod seeded_apps;
pub mod session_package;
mod setup;
mod sheets;
pub mod shell;
mod shortcut_guide;
mod slot_binding;
mod slot_views;
pub mod startup;
mod status;
pub mod surfaces;
pub mod sync;
mod system_sounds;
pub mod termination;
pub mod test_control;
pub mod unlock;
mod wallpapers;
mod window_motion;
mod window_share;
mod window_warmth;
mod workspace_gate;
mod workspace_settings;
mod workspaces;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tilo_actions::remote::InvokeArgument;
use tilo_actions::{ActionAddress, DoorError, ResourceSpec};
use tilo_compositor::{
    Compositor, DisplayConfig, Error, KeyboardLayout, NestedConfig, OutputConfig, WindowPolicy,
};
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
pub use options::{ActionPlane, Machine, Options, no_session_actions};
use options::{open_engine, start_collaboration};
pub use records::Records;
pub use rotation::Accelerometer;
pub use run::{Backend, Reach};
pub use search::{AppActions, SearchOptions};
pub use setup::BeforeDesk;
pub use tilo_service::{Service, Services};

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
use tilo_sheets::Seat as _;
use tilo_sheets::directory::DirectorySeat;
use tilo_sheets::display_confirm::ConfirmSeat;
use tilo_sheets::file_picker::FilePickerSeat;
use tilo_sheets::portal::PortalSeat;
use tilo_sheets::power::{PowerEffect, PowerSeat};
use tilo_sheets::shortcut_guide::GuideSeat;
use tilo_sheets::uninstall::UninstallSeat;
use tilo_sheets::workspace_settings::WorkspaceSettingsSeat;

pub use os_actions::SessionAction;

pub use tilo_onboarding_host::view::OnboardingView;

/// The generation the session's own desktop snapshots carry: a snapshot of
/// an older one is stale.
const GENERATION: &str = "session";

/// How many times the session starts its lock screen for one lock before it
/// lifts the lock: a screen whose app goes is started again, and one that
/// keeps going is not staying up at all — the machine is better unlocked
/// than locked behind it.
const LOCK_SCREEN_TRIES: u32 = 3;

/// How warm the night light makes the displays, in kelvin.
const NIGHT_LIGHT_WARMTH: u32 = 3_400;

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
    /// Where the chrome takes intents the session raises.
    intents: Rc<RefCell<Vec<ChromeIntent>>>,
    /// Whether this session shows quick settings at all: its app is one of the
    /// surface apps it runs, and the bar's status keys open its slot.
    quick_settings: bool,
    /// What the keys asked the panel to show before it ran — the notifications,
    /// the clipboard opened to paste — waiting for its app to be up.
    panel_asks: RefCell<Vec<&'static str>>,
    /// What stands over the windows: its menus, quick settings, the picker
    /// and the rest, and which of them holds the keyboard.
    overlays: Overlays,
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
    /// The readouts of keys whose action a device took and has not answered
    /// yet.
    waiting: RefCell<Vec<keys::PendingReadout>>,
    /// What the services last published of the machine: its connection, its
    /// sound and capture uses, its charge and what plays. The bar's status
    /// cluster, the lock screen and the desk read it.
    readings: status::Shared,
    /// The system services, reached for their readings; their actions go
    /// through the door like every other.
    services: Rc<RefCell<Services>>,
    /// The services the session serves its apps and devices itself, kept for
    /// as long as it runs (`mount::doors::TABLE` says which).
    table: mount::doors::Doors,
    /// The windows this session shares with its workspaces, and the streams it receives from them.
    window_shares: Rc<RefCell<window_share::WindowShares>>,
    /// The runs of what people ask this session's agents to do, fed every
    /// pass who sits here and what the workspace in front admits.
    agent_runs: std::sync::Arc<tilo_agent_runs::AgentRuns>,
    /// Each run's program host, reached by the agent's calls and by the
    /// person's lines in its conversation alike (shell.md §8.1).
    run_programs: run_programs::Shared,
    /// The runs' conversations (shell.md §8.1): a line typed in one is a
    /// program run as the person, or a sentence to the run.
    conversation: std::sync::Arc<conversation::Conversation>,
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
    /// The session another of the person's devices handed this one, as the
    /// now-playing card shows it, until they continue it here: published
    /// beside the person's devices.
    handoff: Rc<std::cell::RefCell<Option<tilo_quick_settings::MediaOffer>>>,
    /// The wakes this session takes part in.
    waking: Rc<mount::wake::Wake>,
    /// What plays, as the desk's music widget shows it: written from the
    /// machine's reading every pass, the same one the bar and the lock screen
    /// show.
    music: tilo_desktop::desk_widgets::DeskMusic,
    /// The power menu, workspace settings, and the questions the session
    /// asks for apps and services, by their seats.
    power: Option<PowerSeat>,
    workspace_settings: Option<WorkspaceSettingsSeat>,
    /// The desk revision workspace settings was last fed at: its panes are
    /// rebuilt when the desk changed under it.
    ws_settings_fed: Cell<Option<u64>>,
    /// The workspace's emoji, which workspace settings shows and changes
    /// through Chat's record of the workspace.
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
    /// The apps' live activities on the bar, which the clock shows and the
    /// controls pressed on it go back to the app through.
    clock: Rc<chrome::Clock>,
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
    observed: RefCell<Option<Vec<(usize, u64)>>>,
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
    /// The person's arrangement, as their own record: what is open where,
    /// with history and undo (shell.md §2). The desk places its windows
    /// from it.
    arrangement: Rc<RefCell<arrangement::Arrangement>>,
    /// Whether the person is mid-gesture or typing, as the compositor last
    /// said: what an arranging action that is not their own waits for.
    underway: Rc<Cell<arrangement::Busy>>,
    /// The heads of the arrangement the file holds, so a record that did not
    /// move is not written again.
    kept_arrangement: RefCell<Option<Vec<ui::automerge::ChangeHash>>>,
    /// Where the arrangement is kept between sessions: this device's state
    /// folder, or nowhere for a session that keeps nothing.
    arrangement_state: Option<std::path::PathBuf>,
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
    /// The installed folder importer's side of this login, once the session
    /// serves it: what it asks on whether first use is over, and the review
    /// it saved to be opened when it is.
    folder_import: RefCell<Option<import_readiness::Continuation>>,
}

/// Type with `keyboard`: the compositor's keymap, and the words printed on
/// the keys of what the session draws (the shortcut guide, the desk's
/// chords), both follow it.
pub(crate) fn apply_keyboard(compositor: &mut Compositor, keyboard: &KeyboardLayout) {
    compositor.set_keyboard_layout(keyboard);
    ui::keycaps::set_active(ui::keycaps::Keycaps::from_xkb(
        &keyboard.layout,
        &keyboard.variant,
    ));
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
    /// The door that decides what a person's line in a run's conversation reads and changes
    /// (shell.md §8.1): each line runs as the person there, in the run's own program host.
    pub(crate) fn lend_conversations(&self, plane: Option<&Plane>, front: &str) {
        // The starter a run's host begins with, as the agent's own `tilo` call uses it: the
        // person's line starts the host itself when it is the run's first program.
        self.conversation
            .lend_starter(std::sync::Arc::new(agent_door::TaskHosts::new(
                &self.person,
                std::sync::Arc::clone(&self.run_programs),
            )));
        // The door's host, over the broker when this session runs one and the session's own
        // doors alone otherwise: a line that reads nothing still runs, and one that reads an app
        // says it cannot here, as the agent's own call would.
        let broker = plane.and_then(Plane::agent_broker);
        self.conversation.lend(
            front,
            std::sync::Arc::new(agent_door::Host::new(
                broker,
                std::sync::Arc::clone(&self.session_doors),
            )),
        );
    }

    /// Keep the person's arrangement in step with their desk: what the desk
    /// did on its own is taken in, and what waited for the person to finish
    /// their gesture or their typing runs once they have.
    fn follow_arrangement(&self, underway: arrangement::Busy) {
        let mut arrangement = self.arrangement.borrow_mut();
        arrangement.observe();
        if !underway.holds() && arrangement.waiting() > 0 {
            if let Err(why) = arrangement.release() {
                tracing::warn!(why, "an arranging action that waited could not be made");
            }
            arrangement.observe();
        }
    }

    /// The workspace in front, as its id: where a chosen notification's app
    /// opens.
    fn active_workspace_id(&self) -> String {
        self.handle.read(|tree| {
            tree.active_workspace()
                .map(|held| held.room().to_string())
                .unwrap_or_default()
        })
    }

    /// Keep the person's arrangement as it stands: its own file, so the next
    /// session places the same desk from it. Written when the record moved,
    /// not every frame.
    fn keep_arrangement(&self) {
        let Some(state) = &self.arrangement_state else {
            return;
        };
        let mut arrangement = self.arrangement.borrow_mut();
        let heads = arrangement.heads();
        if self
            .kept_arrangement
            .borrow()
            .as_ref()
            .is_some_and(|kept| *kept == heads)
        {
            return;
        }
        if let Err(error) = arrangement.save(state) {
            tracing::debug!(%error, "the arrangement is not kept");
            return;
        }
        *self.kept_arrangement.borrow_mut() = Some(heads);
    }

    /// Take up what the workspace's history wrote back and what the person
    /// opened from it, and tell the apps following it what moved.
    fn serve_activity(&self) {
        if let Some(tree) = self.activity.take_restored() {
            self.handle.replace_tree(tree);
        }
        for (workspace, target) in self.activity.take_openings() {
            self.open_from_history(&workspace, &target);
        }
        self.activity.tell_followers();
        // What the history says its changes were, and the names over the runs
        // of them, are written into the desk's document as models answer; the
        // desk draws a frame to read them.
        let described = self.activity.describe();
        if self.activity.take_woken() || described {
            // A window asked the activity model something: the desk draws a
            // frame, which serves it.
            self.desktop.with_widgets(WidgetHost::relayout);
        }
    }

    /// Open what the person chose in the history: the app that holds it,
    /// showing it.
    fn open_from_history(
        &self,
        workspace_id: &tilo_desktop_model::RoomId,
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
        let workspace = apps.workspace_named(&workspace_id.to_string());
        if let Err(error) = apps.start(&launch, &workspace) {
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
        self.session_doors.answer(&self.doors, &self.person);
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
    /// How many times the session started its lock screen for the lock in
    /// force: a screen that keeps going is not staying up at all, and the
    /// lock is lifted rather than left with nothing able to unlock it.
    lock_starts: u32,
    /// The screenshots asked for.
    captures: Option<capture::Captures>,
    /// The capture studio the session shows, and answers the asks of.
    capture_studio: Option<capture_studio::CaptureStudio>,
    /// The windows opening and closing.
    windows: window_motion::WindowMotions,
    /// The temperature and colour every app window is told, as the settings
    /// and wallpapers last said them.
    warmth: window_warmth::Windows,
    /// Whether the night light is on, and whether a change is kept in the
    /// person's settings.
    night_light: (bool, bool),
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
    /// Where quick settings' app stands, while its slot is up, and how often
    /// it was started at that place: one that keeps exiting is given up on.
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
    /// The control sockets `tilo-control` drives this session over, when it
    /// serves any: the development one, and the device admin's own.
    control: Option<test_control::Control>,
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
        // The bar's own setting, read on the same watch: it changes what the
        // desk's band takes and when the bars show.
        let hide_top_bar = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
            settings_store::desktop_controls::DesktopControlsConfig::load().hide_top_bar,
        ));
        let watched_hide = std::sync::Arc::clone(&hide_top_bar);
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
            watched_hide.store(
                settings_store::desktop_controls::DesktopControlsConfig::from_document(&document)
                    .hide_top_bar,
                std::sync::atomic::Ordering::Relaxed,
            );
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
        // The workspace's activity: the desk's own document, and the records
        // its apps keep in the store. The desk records into it, the model
        // reads it, and the apps this session runs reach the same model.
        let activity = activity::Activity::new(
            options.state.as_deref(),
            options.old_desk.as_deref(),
            options.person.clone(),
            options.search.records.as_ref().map(records::Records::root),
            options.search.lists.clone(),
        );
        // When each display's bar shows over the window covering it: the desk's
        // band, the chrome and the input that reaches for the bar read the one
        // model.
        let reveals: Rc<RefCell<tilo_desktop::topbar::Reveals>> = Rc::default();
        // The desk a login shows is built before its first frame: this pair
        // is the phase between the compositor coming up and the frame that
        // draws it (tools/perf/startup_marks.py).
        // The single-threaded start is over: the marks below are written by
        // their own thread, never by the one that builds the desk.
        tilo_startup_metrics::start_writer();
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
                restore = desk_restore::Restore::of(&kept);
                handle.replace_tree(*kept);
            }
            // The person's desk is on disk and this session did not open it:
            // they are looking at a desk that is not theirs, and have to be
            // told where their own still is.
            desk_doc::Kept::Refused(refusal) => {
                notices::tell_desk_not_opened(&notices, &refusal);
            }
            desk_doc::Kept::None => {}
        }
        // The person's arrangement: today's desk, migrated into their own
        // record, and from then on the record the desk places its windows
        // from. It is kept beside the desk on this device, and is the
        // person's own ([`arrangement::record_id`]) on every device theirs.
        // The runs people ask this session's agents for, and each run's program host: the
        // conversation over them (shell.md §8.1) is the shell's, and its lines reach here.
        let the_runs = options.agent_runs.take().unwrap_or_else(|| {
            std::sync::Arc::new(tilo_agent_runs::AgentRuns::new(Some(options.wake.handle())))
        });
        let the_programs = std::sync::Arc::new(run_programs::Programs::new());
        let conversation = std::sync::Arc::new(conversation::Conversation::new(
            std::sync::Arc::clone(&the_runs),
            std::sync::Arc::clone(&the_programs),
            &options.person,
        ));
        let mut record = ui::automerge::AutoCommit::new();
        if let Some(text) = options.state.as_deref().and_then(arrangement::load_at) {
            match ui::automerge::AutoCommit::load(&text) {
                Ok(kept) => record = kept,
                Err(error) => tracing::debug!(%error, "the kept arrangement is not this build's"),
            }
        }
        let kept_desk = handle.read(Clone::clone);
        let records = std::collections::BTreeMap::new();
        let arrangement = Rc::new(RefCell::new(arrangement::Arrangement::new(
            &options.person,
            record,
            handle.clone(),
            &kept_desk,
            &records,
        )));
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
        doors.add(Rc::new(RefCell::new(
            arrangement::ArrangementDoor::new(
                Rc::clone(&arrangement),
                Rc::new(move || underway.get()),
                find.clone(),
            )
            .with_conversation(std::sync::Arc::clone(&conversation)),
        )));
        arrangement.borrow_mut().place_now();
        workspaces::publish_host_actions(&handle);
        // What the person's settings say the desk shows: a device that kept
        // no picture yet takes the choice, and otherwise the workspaces keep
        // the pictures this device kept for them until the setting changes.
        wallpapers.set_active(handle.read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.id.0.to_string())
        }));
        wallpapers.start(&settings_store::load());
        // The apps the person opens read their workspace from this desktop and leave
        // their notice in it.
        if let Some(launch) = &options.launch {
            launch.attach(desktop.clone());
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
        let wells = policy.wells();
        policy.set_window_mapped(Rc::new(move |pid| settling.window_mapped(pid)));
        if let Some(launch) = &options.launch {
            launch.lend_doors(Rc::clone(&window_doors));
        }

        // The programs the person downloaded: the registry takes the
        // windows the desk gives a tab as they appear, so one that names no
        // application directory is offered the first time it shows itself.
        downloaded::install(&mut options, &mut policy);
        let mut compositor = start(policy.boxed())?;
        // A session in a window of another desktop runs no idle timers: that
        // desktop owns idle, blanking, locking and suspend.
        compositor.set_hosted(!options.machine.is_owned());
        // What the services take in wakes the loop instead of waiting for
        // its next pass.
        options.wake.bind(compositor.waker());
        compositor.set_input_config(options.config.input.clone());
        apply_keyboard(&mut compositor, &options.config.keyboard);
        compositor.set_idle_config(options.config.idle);
        a11y_settings::apply(&mut compositor, &options.config.a11y)?;
        compositor.set_backlight(Box::new(display_settings::PanelBacklight::find()));
        let machine = mount::power::install(&mut compositor, &options);
        compositor.add_host(shell::desk_under(options.shell, desktop.clone()));
        // The desk draws every window whole — its card, the lift under it
        // and its edge, in the theme's colours, brighter on the window in
        // use. The compositor shapes a window's corners to that card and
        // draws nothing around it: one drawing, not two edges and two
        // shadows on the same pixel.
        compositor.set_window_frame(tilo_compositor::WindowFrame::corners(f64::from(
            presentation.theme.tokens.radii.tile,
        )));
        let desk_door: tilo_surface_host::Shared = Rc::new(RefCell::new(desktop.clone()));
        doors.add(Rc::clone(&desk_door));
        // The chrome above the windows: its own tree, beside the desk's.
        let intents = Rc::new(RefCell::new(Vec::new()));
        let engine = open_engine(&options, &handle);
        // This device's channels of its workspaces' sealed records: its keys,
        // and the devices each one admits, which its window streams carry
        // between. The broker the apps reach is the one the channel service
        // gates, so a sealed record opens before any of it reaches an app.
        let channels_broker = std::sync::Arc::new(std::sync::Mutex::new(
            tilo_action_broker::collab_core::BrokerChannels::default(),
        ));
        let channel_link = channels::ChannelLink::new();
        let channels = engine.as_ref().and_then(|engine| {
            channels::Channels::open(
                engine,
                std::sync::Arc::clone(&channel_link),
                std::sync::Arc::clone(&channels_broker),
            )
            .map_err(|error| tracing::warn!(%error, "the channel service runs no threads"))
            .ok()
        });
        let sync = start_collaboration(&options, media, engine.clone(), beside);
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
        // The person's private messages, carried by that same endpoint.
        let _messages = mount::services::mount_messages(&mut options, sync.as_deref());
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
        // The machine's status, which the bar's status cluster shows as the
        // battery, network and volume glyphs, the lock screen shows under its
        // card and the desk reads: every part of it is read off the services
        // that publish it.
        let readings = Rc::new(RefCell::new(status::Readings::default()));
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
        // What stands over the windows, opened by the chrome's own widgets,
        // by the session's verbs and by the views bound to them, and read by
        // whoever asks through the door.
        let overlays = Overlays::new();
        let chrome = Rc::new(RefCell::new(chrome::host(
            &handle,
            &options,
            chrome::ChromeWiring {
                intents: Rc::clone(&intents),
                asks: Rc::clone(&asks),
                feeds: chrome::BarFeeds {
                    signals: Rc::clone(&notice_signals),
                    readings: Rc::clone(&readings),
                    clock: Rc::clone(&clock),
                    notices: notices.clone(),
                    tray: tray.clone(),
                    fullscreen: Rc::clone(&fullscreen),
                    shares: Rc::clone(&window_shares),
                    reveals: Rc::clone(&reveals),
                },
            },
            sync.clone(),
            &overview,
            &emoji.link(),
            &overlays,
        )));
        compositor.add_host(shell::bar_under(options.shell, Held(Rc::clone(&chrome))));
        // Search's code is asked for as the session starts, so it is in by the
        // time someone searches; a search asked for before then opens now,
        // and the desk publishes it on its next pass.
        handle.search_palette().borrow_mut().adopt();
        // The desk as the session publishes it, which the shell reads through the door.
        let desk_overlays = Rc::new(overlays::Overlays::new());
        let chrome_door = shell::chrome_door(options.shell, &chrome, &desk_overlays);
        doors.add(Rc::clone(&chrome_door));
        // The runs people ask of this session's agents: the session keeps
        // its own, or plays the run channel a test handed it. Its broker
        // delivers them, it is one of the session's services, and the desk
        // shows each run's conversation.
        let agent_runs = std::sync::Arc::clone(&the_runs);
        let run_programs = std::sync::Arc::clone(&the_programs);
        let desk = || desk_resource::Desk {
            runs: std::sync::Arc::clone(&agent_runs),
            handle: handle.clone(),
            person: options.person.clone(),
            signals: Rc::clone(&notice_signals),
            clock: Rc::clone(&clock),
            status_panel: quick_settings::installed(&options.surfaces),
            panel: overlays.clone(),
            sync: sync.clone(),
            notices: notices.clone(),
            overlays: Rc::clone(&desk_overlays),
            tray: tray.clone(),
            engine: engine.clone(),
        };
        let desk_service = desk();
        let shell = options.shell.and_then(|layer| {
            shell::Shell::mount(
                &mut compositor,
                &mut doors,
                desk(),
                options.scale,
                layer,
                wells.clone(),
                presentation.theme.clone(),
            )
            .map_err(|why| tracing::error!(why, ?layer, "the shell did not start"))
            .ok()
        });
        // A surface the shell's own screen stands in for reads as nothing,
        // as it draws nothing: the shell's parts are the windows a client
        // finds, while the desktop's own desk and chrome keep their actions.
        let aside: Vec<tilo_surface_host::Shared> = if shell.is_some() {
            if options.shell == Some(tilo_surface_host::shell_part::ShellLayer::Screen) {
                vec![Rc::clone(&desk_door), Rc::clone(&chrome_door)]
            } else {
                vec![Rc::clone(&chrome_door)]
            }
        } else {
            Vec::new()
        };
        // Who is here, over the windows: the roster the bar's faces open and
        // where everyone else's pointer is.
        let people = people::mount(&mut compositor, &mut doors, &options, &handle, sync.clone());

        let banners = mount_banners(&mut compositor, &mut doors, &notices, &handle, &options);

        // The sheets, over the desktop and its panels.
        let mount::sheets::Sheets {
            power,
            workspace_settings,
            portal,
            file_picker,
            directory,
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
        let (commands, invocations, triggered, readout_place, readout) = mount_controls(
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
                overlays: overlays.clone(),
                reveals: Rc::clone(&reveals),
            },
            options.scale,
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
        let action_plane = start_action_plane(
            &options,
            &live_activities,
            &handle,
            std::sync::Arc::new(shortcut_library),
            &activity,
            std::sync::Arc::clone(&agent_runs),
            mount::services::Collaboration {
                revocations: std::sync::Arc::clone(&revocations),
                sync: sync.as_deref(),
                engine: engine.as_ref().map(std::sync::Arc::clone),
                sealed: plane::Sealed {
                    broker: std::sync::Arc::clone(&channels_broker),
                    keys: engine.as_ref().map(|engine| engine.keys()),
                },
                content: Some(record_content),
            },
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
        });
        let session_doors = std::sync::Arc::new(agent_door::SessionDoors::new(
            options.wake.handle(),
            std::sync::Arc::clone(&run_programs),
            &options.person,
        ));
        let door = agent_door::mount_named(
            &options,
            questions.clone(),
            std::sync::Arc::clone(&session_doors),
        );
        // The control sockets `tilo-control` reaches, when the session was
        // started with one: the development socket and the device admin's,
        // both answered over the broker this session's plane starts.
        let control = test_control::Control::open(
            &options,
            std::sync::Arc::new(agent_door::Host::new(
                action_plane.as_ref().and_then(Plane::agent_broker),
                std::sync::Arc::clone(&session_doors),
            )),
        );
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
        services
            .borrow_mut()
            .add(Box::new(tilo_agent_runs::Served::new(
                std::sync::Arc::clone(&agent_runs),
            )));
        // The shell's own readings and verbs, on the same door every other
        // service answers on.
        mount::shell::Shell::mount(&services, &handle, overlays.clone());
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

        let search = Rc::new(SessionSearch::new(options.search, options.apps));
        session_search.get_or_init(|| Rc::clone(&search));
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
            earlier: seat.earlier,
            search,
            launch: options.launch,
            restore: RefCell::new(restore),
            drives: options.drives,
            asks,
            intents,
            quick_settings: quick_settings::installed(&options.surfaces),
            panel_asks: RefCell::new(Vec::new()),
            overlays,
            doors,
            invocations,
            triggered,
            commands,
            waiting: RefCell::default(),
            readings,
            services,
            table,
            window_shares,
            agent_runs,
            run_programs,
            conversation,
            hosted: !options.machine.is_owned(),
            fabric,
            vpn,
            devices: mount::devices::Devices::default(),
            handoff: Rc::new(std::cell::RefCell::new(None)),
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
            keyboard,
            owner_session: owner_session::SessionFollow::default(),
            display_confirm,
            notices: notices.clone(),
            clock,
            tray,
            wallpapers,
            lock_layouts: options.config.keyboard_layouts.clone(),
            lock_layout: Cell::new(0),
            session_actions: options.session_actions,
            observed: RefCell::new(None),
            places_seen: RefCell::default(),
            notifications: Rc::new(RefCell::new(notifications::SessionNotifications::new(
                options
                    .desktop
                    .as_ref()
                    .map(|desktop| desktop.notifications().clone()),
                notices,
                banners.clone(),
                notification_policy,
                notice_signals,
                options.power.clone(),
            ))),
            asker,
            agent_door: RefCell::new(agent_door),
            session_doors,
            folder_import: RefCell::new(None),
            sync,
            engine,
            membership_fed: Cell::new(None),
            _channels: channels,
            personal,
            clipboard: Rc::new(mount::clipboard::Clipboard::new()),
            arrangement,
            underway: busy,
            kept_arrangement: RefCell::new(None),
            arrangement_state: options.state.clone(),
            settings: mount::settings_sync::SettingsSync::new(),
            overview,
            dictation,
            emoji,
            shortcut_guide,
            chrome,
            people,
            fullscreen,
            hide_top_bar,
            controller,
            controller_keys,
            app_sheets,
        };
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
        let slot_views = slot_views::SlotViews::new(shared.overlays.clone(), options.scale);
        // Assistive technology reads the desktop through the same endpoint an
        // app's window has: what a client asks wakes this loop, which is
        // where every tree and every action lives.
        let wake = loop_wake.clone();
        let a11y = a11y::Accessibility::new(Arc::new(move || wake.wake_by_ref()), aside);
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
            lock_starts: 0,
            captures: options.captures,
            capture_studio,
            windows: window_motion::WindowMotions::new((options.presentation)().theme.motion()),
            warmth,
            night_light: (options.config.night_light, options.config.kept),
            keyboard_preference: options.config.on_screen_keyboard,
            keyboard_run: keyboard::Run::default(),
            keyboard_starts: 0,
            quick_settings_run: quick_settings::Run::Down,
            quick_settings_starts: 0,
            portal: options.portal,
            launches: std::sync::Arc::clone(&options.launches),
            action_plane,
            control,
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
        if self.night_light.0 {
            self.compositor.set_night_light(Some(NIGHT_LIGHT_WARMTH))?;
        }
        self.follow_displays()?;
        self.follow_fullscreen();
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
        let night_light = self.night_light.0.then_some(NIGHT_LIGHT_WARMTH);
        let turned = self.rotation.follow(&self.compositor.outputs());
        // What the displays service publishes follows what the compositor
        // shows: which ones there are, and which one the person works on.
        self.displays_service
            .land(&self.shared.handle, &self.compositor.outputs());
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
        self.shared.clock.board()
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

    /// The person's arrangement, as their own record.
    #[must_use]
    pub fn arrangement(&self) -> &Rc<RefCell<arrangement::Arrangement>> {
        &self.shared.arrangement
    }

    /// Undo the person's last arranging step, through the published action
    /// as any caller does.
    ///
    /// # Errors
    /// The door's refusal: there is no step to take back.
    pub fn undo_arrangement(&mut self) -> Result<(), DoorError> {
        let address = ActionAddress::new(arrangement::RECORD_RESOURCE, "undo");
        self.shared
            .doors
            .invoke(&address, &serde_json::Map::new())?;
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
        self.shared
            .doors
            .invoke_as(actor, &ActionAddress::new(resource, action), arguments)
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

    /// The power menu's seat: the session puts it up.
    #[must_use]
    pub fn power_menu(&self) -> Option<&PowerSeat> {
        self.shared.power.as_ref()
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
        let continuation = import_readiness::Continuation::serve(display)?;
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
            self.follow_app_asks();
            self.shared.follow_gestures();
            self.shared.follow_tab_menus();
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
        Ok(())
    }

    /// Answer what the apps asked of their own tabs, and of what a capture
    /// may show of them, as the desk commands they are.
    fn follow_app_asks(&mut self) {
        if let Some(tabs) = &mut self.tabs {
            tabs.follow(
                &self.shared.desktop,
                self.shared.launch.as_deref(),
                &mut self.compositor,
                self.desktop.as_deref(),
                self.capture_studio.as_ref(),
            );
        }
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
        self.shared.follow_records();
        // Who each workspace on the desk is whose, after the workspaces this
        // pass brought: the person's own are theirs, and one joined keeps its
        // host's owner.
        self.shared.own_workspaces();
        self.follow_power()?;
        if let Some(comforts) = &mut self.comforts {
            comforts.observe();
        }
        self.follow_owner_session()?;
        self.follow_lock()?;
        self.follow_keyboard();
        // The engine of the active source, after the on-screen keyboard gave
        // up the seat: one input method owns it.
        self.follow_input_method();
        // Quick settings runs while the stack has its slot up, where the
        // stack placed it.
        self.follow_quick_settings();
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
        self.place_sheets();
        self.settle_asks()?;
        // The menu the reading says is up has its view over the desk, and
        // the view goes when the menu does.
        self.follow_window_menu();
        // Each open window takes the temperature of the workspace it is in, as this pass left it.
        self.follow_window_warmth();
        self.shared.clipboard.serve_asks(&mut self.compositor);
        // A paste of files runs on its own thread; the loop only asks
        // whether it is over and tells the person how it went.
        for notice in self.shared.clipboard.step_files() {
            self.shared.notifications.borrow().tell(notice);
        }
        // What was copied here reaches the person's other devices, and what
        // they copied there joins the clipboard here.
        if let Some(fabric) = &self.shared.fabric {
            let media_enabled = fabric.require_media().is_ok();
            self.shared.clipboard.sync_fabric(fabric.as_ref());
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
            self.shared.follow_device_vpn(fabric);
            // What this pass offered the clipboard, and what the pumps just
            // changed, is this device's next word to its other devices.
            if let Some(sync) = &self.shared.sync {
                sync.announce_fabric();
            }
        }
        Ok(())
    }

    /// Keep the workspaces this session takes part in current with the
    /// desktop's, and hand the desktop who is here.
    fn follow_collaboration(&self) {
        let Some(sync) = &self.shared.sync else {
            return;
        };
        self.shared.handle.read(|tree| sync.follow_desktop(tree));
        sync.follow();
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
        if self.lock_screen().is_none() {
            tracing::warn!("no lock screen is installed: not locking");
            return false;
        }
        true
    }

    /// Show a mode change in the readout, said in words: a switch flipped by
    /// a key or a switch has nothing to read as a level.
    fn hint(&self, kind: OsdKind, on: bool) {
        self.show_hint(Hint {
            kind,
            on,
            chord: None,
            name: None,
        });
    }

    /// Show `hint` in the readout.
    fn show_hint(&self, hint: Hint) {
        self.shared.show_readout(&Osd::from(hint));
    }

    fn command(&mut self, command: SessionCommand) -> Result<(), Error> {
        match command {
            SessionCommand::Lock => self.lock_by_person(),
            SessionCommand::OpenPowerMenu => {
                if let Some(power) = &self.shared.power {
                    power.presentation().open(false);
                }
                Ok(())
            }
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
                self.hint(
                    OsdKind::Magnifier,
                    self.magnifier.0 > settings_store::magnifier::OFF,
                );
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
                self.hint(OsdKind::NightLight, on);
                Ok(())
            }
            SessionCommand::PasteFiles { destination } => {
                self.paste_files(&destination);
                Ok(())
            }
            SessionCommand::NextInputSource => {
                let Some((sources, name)) = input_sources::next() else {
                    return Ok(());
                };
                self.sources = sources;
                apply_keyboard(&mut self.compositor, &self.sources.compiled());
                self.show_hint(Hint {
                    kind: OsdKind::InputSource,
                    on: true,
                    chord: None,
                    name: Some(name),
                });
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
                self.hint(OsdKind::Touchpad, enabled);
                Ok(())
            }
        }
    }

    /// Paste what was copied into `destination`, beside what is already there.
    fn paste_files(&self, destination: &str) {
        self.shared.clipboard.paste_files(
            std::path::Path::new(destination),
            crate::mount::clipboard_files::Conflict::KeepBoth,
        );
    }

    /// Answer the installed folder importer this pass, and open the review it
    /// saved once the person's first use is over: the Add folders click they
    /// left for after setup rather than one they must remember.
    fn follow_folder_import(&self) {
        let first_use_over = !self.setup_up();
        let opens_now = self
            .shared
            .folder_import
            .borrow_mut()
            .as_mut()
            .is_some_and(|import| import.follow(first_use_over));
        if !opens_now {
            return;
        }
        let Some(apps) = &self.shared.launch else {
            return;
        };
        let launch = tilo_app_launch::Launch::Host {
            desktop_id: import_readiness::IMPORT_DESKTOP_ID.to_owned(),
            open: None,
        };
        if let Err(error) = apps.open(&launch) {
            tracing::warn!(
                %error,
                "the saved folder import did not open; Add folders opens it"
            );
        }
    }

    /// What the person is writing with besides the keyboard: dictation and
    /// the emoji picker, both through the field they are typing in.
    fn follow_text_inputs(&mut self) {
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
        quick_settings::follow_dictation(&self.shared);
        // The picker's picks go into the field the person was typing in,
        // through the same text-input door dictation writes through.
        let origin = self
            .shared
            .desktop
            .with_widgets(|host| host.placement().origin);
        self.shared
            .emoji
            .step(&mut self.compositor, origin, Instant::now());
    }

    /// The notifications that arrived this pass, and the flash a person who
    /// asked for visual alerts sees for them.
    fn follow_notifications(&mut self) {
        let compositor = &mut self.compositor;
        let appeared = self
            .shared
            .notifications
            .borrow_mut()
            .step(&mut || Some(compositor.activation_token()));
        // What the person was just told flashes the screen when they asked
        // for a visual alert: a person who cannot hear the alert sound is
        // told of it the same way.
        if appeared && compositor.a11y().visual_alerts {
            compositor.flash();
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
        // Add folders: answered whether the identity ceremony is over, and the
        // review the person left opens once it is.
        self.follow_folder_import();
        // What the person is in the middle of, as the desk may not move
        // under it (shell.md §1).
        let underway = self.compositor.underway();
        self.shared.underway.set(underway);
        self.shared.follow_arrangement(underway);
        self.follow_controllers()?;
        self.follow_displays()?;
        self.follow_fullscreen();
        self.follow_overlays();
        self.follow_setup();
        {
            self.shared.feed_agent_runs();
            // The door that decides what a person's line in a run's conversation reads and
            // changes, once this pass's runs know where they are (shell.md §8.1).
            {
                let front = self.shared.handle.read(|tree| {
                    tree.active_workspace()
                        .map(|held| held.room().to_string())
                        .unwrap_or_default()
                });
                self.shared
                    .lend_conversations(self.action_plane.as_ref(), &front);
            }
            let mut services = self.shared.services.borrow_mut();
            services.poll();
            if let Some(plane) = &self.action_plane {
                plane.link.serve(&mut services);
            }
        }
        // What the services published this pass is what every surface of the
        // machine shows.
        self.shared.follow_status();
        self.follow_readout();
        self.shared.show_answered_readouts();
        // The surface apps are answered after the services took in what
        // their devices did, so a panel is sent the reading of this pass.
        self.surfaces.poll();
        self.surfaces.serve(&mut self.shared.doors);
        // The windows shared with the workspaces: what the person asked,
        // what the plane brought, and the frames of the ones on screen.
        {
            let sync = self.shared.sync.clone();
            let displays = &self.displays_service;
            self.shared.window_shares.borrow_mut().step(
                &mut self.compositor,
                &self.shared.handle,
                displays,
                sync.as_deref(),
                std::time::Instant::now(),
            );
        }
        self.follow_text_inputs();
        self.follow_notifications();
        self.shared.open_notifications();
        self.settle()?;
        self.follow_bars();
        self.follow_shell()?;
        self.follow_slot_views()?;
        // The toolkit's clock, which a capture pins the frame's instant on.
        let now = ui::time::now();
        self.windows.step(&mut self.compositor, now);
        // The overview's windows fly between the desk and the overview: its
        // panel names them, the desk says where each stands, and the
        // compositor draws their own pixels.
        self.shared.overview.step(&self.shared, now);
        self.compositor.render()?;
        // What `tilo-control` asks of the frame just drawn.
        let locked = self.compositor.locked();
        let lock_screen_drawn = self.compositor.lock_drawn();
        test_control::serve(
            self.control.as_ref(),
            &mut self.compositor,
            test_control::Desk {
                doors: &self.shared.doors,
                handle: &self.shared.handle,
                overlays: &self.shared.overlays,
                engine: self.shared.engine.as_deref(),
                locked,
                lock_screen_drawn,
            },
        );
        // What assistive technology reads of the frame just drawn, and what
        // it asked of it.
        self.a11y.step(&self.shared.doors);
        // A live projection is sent the desktop as the pass just drew it.
        let video = self.display_sheet.video_stream();
        self.projection_video
            .follow(&mut self.compositor, video.as_ref());
        // A call shares the same pass: it takes the frame when it wants one,
        // and a locked desktop hands it nothing.
        crate::calls::share_screen(&mut self.compositor);
        self.follow_shell()?;
        self.follow_slot_views()?;
        self.settle()?;
        self.shared.observe();
        self.shared.keep_desk();
        self.shared.keep_arrangement();
        // The picture is the workspace in front's, which a change covering one workspace lands on.
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
        // Its apps' display goes with it, and so do they: the apps outlive
        // no session, whoever else still holds what started them.
        if let Some(launch) = &self.shared.launch {
            launch.end_running();
        }
    }
}
