//! The session's state between frames, as every host runs it: what its roles keep — the desk and
//! the arrangement it holds, the records, the people, what search reads, the notices and the
//! workspaces' handles — beside the effects a host provides, which only its own host names: the
//! compositor and the services of a Linux login (`platform::session`), the page's own on the web.
//!
//! A role in this crate takes `&Shared` or `&mut Session` and reaches an effect only through a
//! field that every host has; what only one host can do stays behind `cfg(target_os = "linux")`
//! here until its concern has a seam in [`crate::platform`].

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Instant;

use tilo_desktop::chrome::ChromeIntent;
use tilo_desktop::desktop_host::DesktopAsk;
use tilo_desktop::overlays::Overlays;
use tilo_desktop_surface::DesktopHandle;

/// Where each published part stood when search last read it: the part's
/// index and its revision and head.
pub(crate) type Observed = RefCell<Option<Vec<(usize, (u64, u64))>>>;

#[cfg_attr(
    target_arch = "wasm32",
    allow(
        dead_code,
        reason = "the page builds its own host over this state next"
    )
)]
/// What the session keeps between frames, shared with the loop.
pub(crate) struct Shared {
    pub(crate) handle: DesktopHandle,
    #[cfg(target_os = "linux")]
    pub(crate) desktop: crate::platform::surfaces::DesktopHost,
    /// The workspace's activity: the desk's changes, and the records its
    /// apps keep.
    #[cfg(target_os = "linux")]
    pub(crate) activity: crate::activity::Activity,
    /// Who sits here, as the desk's rules and the workspaces' governance
    /// name them.
    pub(crate) person: String,
    /// The ids they went by before this device had an identity, which their
    /// own workspaces may still name them by.
    pub(crate) earlier: Vec<String>,
    #[cfg(target_os = "linux")]
    pub(crate) search: Rc<crate::search::SessionSearch>,
    /// The apps the person opens, when this session starts them.
    #[cfg(target_os = "linux")]
    pub(crate) launch: Option<Rc<crate::apps::Apps>>,
    /// The windows this device left open, asked for again one at a time into
    /// the tabs they were in.
    #[cfg(target_os = "linux")]
    pub(crate) restore: RefCell<crate::desk_restore::Restore>,
    /// The drives the person plugs in, when this session watches them.
    #[cfg(target_os = "linux")]
    pub(crate) drives: Option<Rc<crate::drives::Drives>>,
    /// What the desk and the chrome asked of the session.
    pub(crate) asks: Rc<RefCell<Vec<DesktopAsk>>>,
    /// Whether the shell's send key was pressed, waiting for the pass that
    /// asks the window in front to open its box. The key can arrive while the
    /// desk is between two of its own frames, so the window it was over is
    /// named then, by the desk, and never at the keypress.
    pub(crate) send_from: Cell<bool>,
    /// Whether the shell's "report a problem" was asked for, waiting for the
    /// pass that asks the window in front to open its box to the one place
    /// this computer sends reports; and where that place is, when this
    /// computer is told.
    pub(crate) report_from: Cell<bool>,
    #[cfg(target_os = "linux")]
    pub(crate) feedback: Option<crate::feedback::Destination>,
    /// Where the chrome takes intents the session raises.
    pub(crate) intents: Rc<RefCell<Vec<ChromeIntent>>>,
    /// Whether this session shows quick settings at all: its app is one of the
    /// surface apps it runs, and the bar's status keys open its slot.
    pub(crate) quick_settings: bool,
    /// What the keys asked the panel to show before it ran — the notifications,
    /// the clipboard opened to paste, the page Escape goes back from — waiting
    /// for its app to be up.
    pub(crate) panel_asks: Rc<RefCell<Vec<&'static str>>>,
    /// The ask the panel goes back on, while it has not answered whether it
    /// had anywhere to go back to: Escape closes the panel only from its main
    /// page, and the panel says which page that is.
    pub(crate) panel_back: Cell<Option<u64>>,
    /// What stands over the windows: its menus, quick settings, the picker
    /// and the rest, and which of them holds the keyboard.
    pub(crate) overlays: Overlays,
    /// The two stacks the surfaces stand in, as one: what the shell's views
    /// and the desk's own popovers open through, so one of them closes the
    /// others.
    #[cfg(target_os = "linux")]
    pub(crate) surfaces: crate::desk_resource::Surfaces,
    /// The desk's own overlays, as its reading and its views name them: the
    /// surface up, and Alt+Tab's walk.
    pub(crate) desk_overlays: Rc<crate::overlays::Overlays>,
    /// Alt+Tab's panel, shared by the desk's walk (which sets it), the chrome
    /// (which draws it) and the compositor (which pictures its windows).
    pub(crate) switcher: tilo_desktop::window_switcher::Switcher,
    /// Every hosted surface's and service's published side: the session's
    /// action host.
    pub(crate) doors: tilo_session_core::door::Doors,
    /// Published actions keys and gestures asked for, waiting for the door.
    pub(crate) invocations: crate::keys::Invocations,
    /// Chords pressed that run a saved shortcut, waiting for the automation
    /// host.
    pub(crate) triggered: crate::keys::PressedChords,
    /// What the session's own resources asked of the compositor.
    #[cfg(target_os = "linux")]
    pub(crate) commands: Rc<RefCell<Vec<crate::controls::SessionCommand>>>,
    /// What the session's own resources read of this device.
    #[cfg(target_os = "linux")]
    pub(crate) controls: Rc<RefCell<crate::controls::ControlsReading>>,
    /// The readouts of keys whose action a device took and has not answered
    /// yet.
    #[cfg(target_os = "linux")]
    pub(crate) waiting: RefCell<Vec<crate::platform::keys::PendingReadout>>,
    /// What the services last published of the machine: its connection, its
    /// sound and capture uses, its charge and what plays. The bar's status
    /// cluster, the lock screen and the desk read it.
    pub(crate) readings: crate::status::Shared,
    /// The system services, reached for their readings.
    pub(crate) services: Rc<RefCell<tilo_service::Services>>,
    /// The sends the session's send action took, which the pass carries out.
    #[cfg(target_os = "linux")]
    pub(crate) sends: crate::send_to::Sends,
    /// The services the session serves its apps itself.
    #[cfg(target_os = "linux")]
    pub(crate) table: crate::doors::Doors,
    /// The windows this session shares with its workspaces, and the streams it receives from them.
    #[cfg(target_os = "linux")]
    pub(crate) window_shares: Rc<RefCell<crate::window_share::WindowShares>>,
    /// The windows whose apps hand over their page as a paint stream: what
    /// this session publishes on the workspace's live plane while someone
    /// shows them.
    #[cfg(target_os = "linux")]
    pub(crate) pages: Rc<RefCell<crate::page_track::PageTracks>>,
    /// The turns this session's agents answer: a person's message in a
    /// conversation the agent takes part in, read every pass from Chat's
    /// record of each workspace.
    #[cfg(target_os = "linux")]
    pub(crate) conversations: std::rc::Rc<crate::conversations::Conversations>,
    /// The agents each workspace keeps, read every pass from its record of
    /// them: who keeps an agent is that record's word, and what the broker
    /// admits an agent's session on.
    #[cfg(target_os = "linux")]
    pub(crate) agents: std::rc::Rc<crate::agents::Agents>,
    /// The agents the person admitted at its agent door, as the desk and the bar show them.
    #[cfg(target_os = "linux")]
    pub(crate) seats: std::sync::Arc<crate::agent_door::Seats>,
    /// It runs in a window of a desktop it did not start (see
    /// [`Options::machine`]).
    pub(crate) hosted: bool,
    /// The person's device fabric, while this session runs on one.
    #[cfg(target_os = "linux")]
    pub(crate) fabric: Option<crate::platform::fabric::SharedLink>,
    /// The device VPN, while this session runs on a machine with the helper:
    /// its profiles' kill switches, and the route through another of the
    /// person's devices, which follows the pass.
    #[cfg(target_os = "linux")]
    pub(crate) vpn: Option<crate::vpn::Routes>,
    /// The person's devices as the panel was last fed them.
    #[cfg(target_os = "linux")]
    pub(crate) devices: crate::devices::press::Devices,
    /// Moving a Bluetooth accessory between the person's devices: what the
    /// panel asked for, what their other devices said, and the Bluetooth
    /// work the move needs.
    #[cfg(target_os = "linux")]
    pub(crate) bluetooth: crate::bluetooth_handoff::Handoffs,
    /// The session another of the person's devices handed this one, as the
    /// now-playing card shows it, until they continue it here: published
    /// beside the person's devices.
    #[cfg(target_os = "linux")]
    pub(crate) handoff: Rc<std::cell::RefCell<Option<tilo_quick_settings::MediaOffer>>>,
    /// The wakes this session takes part in.
    #[cfg(target_os = "linux")]
    pub(crate) waking: Rc<crate::wake::Wake>,
    pub(crate) wake_handle: tilo_service::Wake,
    /// What plays, as the desk's music widget shows it.
    pub(crate) music: tilo_desktop::desk_widgets::DeskMusic,
    /// The power menu, workspace settings, the questions the session asks
    /// for apps and services, and the one deleting a workspace asks, by
    /// their seats.
    #[cfg(target_os = "linux")]
    pub(crate) power: Option<tilo_sheets::power::PowerSeat>,
    #[cfg(target_os = "linux")]
    pub(crate) workspace_settings: Option<tilo_sheets::workspace_settings::WorkspaceSettingsSeat>,
    /// The desk revision workspace settings was last fed at: its panes are
    /// rebuilt when the desk changed under it.
    pub(crate) ws_settings_fed: Cell<Option<u64>>,
    /// The desk revision the file access list and what apps may capture were
    /// last worked out at: both follow every change of the desk, so a
    /// revocation reaches the running app at once.
    pub(crate) capabilities_fed: Cell<Option<u64>>,
    /// What publishes the person's file access list to the Files daemon
    /// while the session runs; `None` where no Files workspace serves.
    #[cfg(target_os = "linux")]
    pub(crate) fs_authority: Option<crate::fs_authority::Publisher>,
    /// The workspace's agents and emoji, which workspace settings shows and
    /// changes through Chat's record of the workspace.
    #[cfg(target_os = "linux")]
    pub(crate) workspace_tabs: crate::workspace_settings::WorkspaceTabs,
    #[cfg(target_os = "linux")]
    pub(crate) portal: Option<tilo_sheets::portal::PortalSeat>,
    #[cfg(target_os = "linux")]
    pub(crate) file_picker: Option<tilo_sheets::file_picker::FilePickerSeat>,
    /// The question about a display change, which stands on every display
    /// at once so it is seen even when the change blanked one of them.
    #[cfg(target_os = "linux")]
    pub(crate) display_confirm: Option<tilo_sheets::display_confirm::ConfirmSeat>,
    /// The workspaces this computer may join or open.
    #[cfg(target_os = "linux")]
    pub(crate) directory: Option<tilo_sheets::directory::DirectorySeat>,
    /// Inviting people: the sheet a link into the workspace in front is made
    /// from, and what it made in this opening.
    pub(crate) invite: Option<tilo_sheets::invite::InviteSeat>,
    pub(crate) inviting: crate::invite::Invite,
    /// The question deleting a workspace asks, and the workspace it is about.
    #[cfg(target_os = "linux")]
    pub(crate) delete_workspace: Option<tilo_sheets::delete_workspace::DeleteWorkspaceSeat>,
    pub(crate) delete_workspace_ask: RefCell<Option<tilo_desktop_model::WorkspaceSlot>>,
    /// What the sites this computer knows list as public workspaces, read
    /// on a worker of its own.
    #[cfg(target_os = "linux")]
    pub(crate) directory_store: Rc<crate::directory::Store>,
    /// The workspace the host started this session to join.
    #[cfg(target_os = "linux")]
    pub(crate) boot_join: RefCell<crate::boot_join::BootJoin>,
    /// The workspace's Browser this session holds, when a host started it for one: the workspace
    /// it joins, the record whose window it opens, and what it reports of it
    /// ([`crate::hosted_browser::HostedBrowser`]).
    #[cfg(target_os = "linux")]
    pub(crate) hosted_browser: RefCell<Option<crate::hosted_browser::HostedBrowser>>,
    /// The community site this computer is configured with, joined on its
    /// own as the session starts.
    #[cfg(target_os = "linux")]
    pub(crate) community: RefCell<crate::community::Community>,
    /// The lock, as the session publishes it for the lock screen to show.
    #[cfg(target_os = "linux")]
    pub(crate) lock: crate::lock_service::Lock,
    /// The on-screen keyboard, as the session publishes it for its app to
    /// show where the person types.
    #[cfg(target_os = "linux")]
    pub(crate) keyboard: crate::keyboard::Keyboard,
    /// This device's side of the person's session on their devices: what it
    /// says of its own lock and what it has followed from theirs.
    #[cfg(target_os = "linux")]
    pub(crate) owner_session: crate::owner_session::SessionFollow,
    /// The desktop's own notices: what the session tells the person, such as
    /// leaving a workspace.
    pub(crate) notices: tilo_desktop_surface::DesktopNotifications,
    /// The windows carried to another workspace, asked about first.
    #[cfg(target_os = "linux")]
    pub(crate) moves: crate::window_move::Moves,
    /// The workspaces being branched, put on the desk once what they hold is copied.
    #[cfg(target_os = "linux")]
    pub(crate) branches: crate::workspace_branch::Branches,
    /// The apps' live activities on the bar, which the pill names and the
    /// controls pressed on it go back to the app through.
    #[cfg(target_os = "linux")]
    pub(crate) activities: Rc<crate::chrome::Activities>,
    /// The running applications' tray items, which the bar shows and whose
    /// clicks the session hands back to the applications.
    #[cfg(target_os = "linux")]
    pub(crate) tray: Option<std::sync::Arc<tilo_tray_host::Tray>>,
    /// The pictures the desk shows, by workspace.
    #[cfg(target_os = "linux")]
    pub(crate) wallpapers: std::sync::Arc<crate::wallpapers::Wallpapers>,
    /// The keyboard layouts the person switches between, and which one is
    /// in use, as the lock screen shows them.
    pub(crate) lock_layouts: Vec<String>,
    pub(crate) lock_layout: Cell<usize>,
    #[cfg(target_os = "linux")]
    pub(crate) session_actions: fn(os_actions::SessionAction) -> Result<(), String>,
    /// Where everything the session publishes stood when search last read
    /// it.
    pub(crate) observed: Observed,
    /// The second search last read who is here: people and the calls they are
    /// in change with the clock as well as with what arrives.
    pub(crate) here_at: Cell<u64>,
    /// What presence stood at when search last read who is here, so a person
    /// arriving inside that second is listed at once.
    pub(crate) here_seen: Cell<u64>,
    /// The title each window had when search last recorded where it was.
    pub(crate) places_seen: RefCell<std::collections::HashMap<u64, String>>,
    /// The daemon's notifications on the desktop, as the session publishes
    /// the centre they are kept in.
    #[cfg(target_os = "linux")]
    pub(crate) notifications: Rc<RefCell<crate::notifications::SessionNotifications>>,
    /// The person's mentions in the workspaces' records, and what they chose of them.
    #[cfg(target_os = "linux")]
    pub(crate) mentions: Option<crate::mentions::Mentions>,
    /// What the session opens an app through: a search row, a link, the place a notice names.
    #[cfg(target_os = "linux")]
    pub(crate) apps: Option<Rc<dyn crate::search::AppActions>>,
    /// What the services ask the person, on the sheets.
    #[cfg(target_os = "linux")]
    pub(crate) asker: crate::questions::Asker,
    /// Who is here: this session's membership of each workspace, when it
    /// takes part in the collaboration plane.
    pub(crate) sync: Option<Rc<crate::sync::Client>>,
    /// This device's workspaces: what the desk shows of them, and what a join takes in.
    pub(crate) engine: Option<std::sync::Arc<crate::engine::Engine>>,
    /// The workspaces' shared desktops, as their records hold them: what each
    /// device that shows a workspace puts on its own desk, and what this one
    /// writes back.
    #[cfg(target_os = "linux")]
    pub(crate) desktops: crate::shared_desktop::SharedDesktops,
    /// What each window this session runs shows, read from the broker every
    /// pass: the desk's reading of it, and the record noted beside the window
    /// in its workspace's layout.
    pub(crate) notes: std::sync::Arc<crate::continuations::Continuations>,
    /// The engine revision the desk's copy of each workspace's people was
    /// last taken from: what nothing new of a workspace's governance has
    /// passed since.
    pub(crate) membership_fed: Cell<Option<u64>>,
    /// This device's channels of those workspaces' sealed records, and the
    /// devices each one admits: kept for as long as the session runs.
    #[cfg(target_os = "linux")]
    pub(crate) _channels: Option<crate::channels::Channels>,
    /// The person's own workspaces, as all their devices hold the list of
    /// them: `None` when this computer has no identity to keep it for.
    #[cfg(target_os = "linux")]
    pub(crate) personal: Option<RefCell<crate::workspaces::Personal>>,
    /// The copies the person can paste again.
    #[cfg(target_os = "linux")]
    pub(crate) clipboard: Rc<crate::clipboard::Clipboard>,
    /// The person's arrangement: the desk this session holds, with history and
    /// undo (shell.md §2), and the app places its windows from it.
    pub(crate) arrangement: Rc<RefCell<crate::arrangement::Arrangement>>,
    /// Whether the person is mid-gesture or typing, as the compositor last
    /// said: what an arranging action that is not their own waits for.
    pub(crate) underway: Rc<Cell<tilo_session_core::arrangement::Busy>>,
    /// The person's settings, kept in step with their other devices.
    #[cfg(target_os = "linux")]
    pub(crate) settings: crate::settings_sync::SettingsSync,
    /// A joined device's certificate, renewed through the person's devices
    /// once it is due: this device holds no root to reissue it with.
    #[cfg(target_os = "linux")]
    pub(crate) fleet_renewal: Option<std::sync::Arc<crate::renewal::FleetRenewal>>,
    /// The overview's pictures and the clock they fly on, which its panel
    /// in the chrome draws with.
    pub(crate) overview: crate::overview::Overview,
    /// Dictation: the speech the person allows once per dictation, and the
    /// sheet they answer on.
    #[cfg(target_os = "linux")]
    pub(crate) dictation: crate::dictation::Dictation,
    /// The emoji picker's picks: written into the field the person was typing
    /// into, and where that field's caret stands for the card to open at.
    #[cfg(target_os = "linux")]
    pub(crate) emoji: crate::emoji::Emoji,
    /// What the desktop's control hints read from this host and give back: the person's record,
    /// the reasons this session knows, and what they learned.
    pub(crate) hints: tilo_desktop::control_hints::HintsFeed,
    /// The person's settings as the hints' record was last taken from them.
    pub(crate) hints_stamp: Cell<Option<settings_store::SettingsStamp>>,
    /// The shortcut guide: the sheet naming every key the desktop answers,
    /// and the table it explains.
    #[cfg(target_os = "linux")]
    pub(crate) shortcut_guide: crate::shortcut_guide::ShortcutGuide,
    /// The chrome above the windows as the session hosts it: where its
    /// bars stand on the displays, read back for the tests that drive them.
    #[cfg(target_os = "linux")]
    pub(crate) chrome: Rc<RefCell<crate::platform::surfaces::WidgetHost>>,
    /// Who is here, as the bar's faces open it: the roster it shows and
    /// where everyone else's pointer is.
    #[cfg(target_os = "linux")]
    pub(crate) people: Rc<RefCell<crate::people::People>>,
    /// Whose view this device follows — its own presence word — and which of their place it
    /// already brought forward.
    pub(crate) follow: RefCell<tilo_session_core::presence::Follow>,
    /// The monitors of the displays a fullscreen window owns, which the
    /// bars read.
    pub(crate) fullscreen: Rc<RefCell<Vec<String>>>,
    /// Whether the person may invite someone into the workspace in front, as
    /// the loop last answered it: the bar's invite button and the roster's
    /// footer are offered on it.
    pub(crate) invitable: Rc<Cell<bool>>,
    /// The person's "hide the top bar until the pointer is near it", as the
    /// settings watch last read it.
    pub(crate) hide_top_bar: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// The controller the person last drove the desktop with: the guide
    /// leads with its buttons until a key of their own is pressed.
    pub(crate) controller: Rc<Cell<Option<ui::ControllerFamily>>>,
    /// Whether the keys being routed now are a controller's own, standing
    /// for its presses: those say nothing about a keyboard.
    pub(crate) controller_keys: Rc<Cell<bool>>,
    /// The questions about the person's apps: whether to add a program they
    /// downloaded to their apps, and whether to uninstall one.
    #[cfg(target_os = "linux")]
    pub(crate) app_sheets: crate::app_sheets::AppSheets,
    /// The agent door, when this session serves one: what a program of this
    /// login asks on, and the person's answers about it.
    #[cfg(target_os = "linux")]
    pub(crate) agent_door: RefCell<Option<crate::agent_door::Loop>>,
    /// What a connection of any door asks of the session's own doors: answered
    /// on the loop's pass, the only place that may touch them.
    #[cfg(target_os = "linux")]
    pub(crate) session_doors: std::sync::Arc<crate::agent_door::SessionDoors>,
    /// The agent door toward the person's other devices.
    #[cfg(target_os = "linux")]
    pub(crate) remote: std::sync::Arc<crate::remote_door::Remote>,
    /// The installed folder importer's side of this login, once the session
    /// serves it: what it asks on whether first use is over, and the review
    /// it saved to be opened when it is.
    #[cfg(target_os = "linux")]
    pub(crate) folder_import: RefCell<Option<crate::import_readiness::Continuation>>,
}

#[cfg_attr(
    target_arch = "wasm32",
    allow(
        dead_code,
        reason = "the page builds its own host over this state next"
    )
)]
/// A running session: the compositor and the desktop it hosts.
pub struct Session {
    #[cfg(target_os = "linux")]
    pub(crate) compositor: tilo_compositor::Compositor,
    pub(crate) shared: Shared,
    #[cfg(target_os = "linux")]
    pub(crate) banners: tilo_notification_banners::Link,
    /// The input preferences the compositor runs with now.
    #[cfg(target_os = "linux")]
    pub(crate) input: tilo_compositor::InputConfig,
    /// The person logged out: the loop ends.
    pub(crate) ended: bool,
    /// The first frame reached the display: the startup is measured and
    /// closed.
    pub(crate) first_frame: bool,
    /// The workspace in front, as this pass last read it: a change is a
    /// switch to time.
    pub(crate) active_workspace: Option<u64>,
    /// The session's own actions, as the broker's steps: what each pass hands the rows it read.
    #[cfg(target_os = "linux")]
    pub(crate) published_steps: Option<std::sync::Arc<crate::session_actions::SessionActions>>,
    /// When the desk's own bookkeeping last read it.
    pub(crate) kept_at: Instant,
    /// What the desk's input a tool handed the loop cost this pass, read with
    /// the control socket's own time in the frame's timings.
    pub(crate) tool_input: std::time::Duration,
    /// The magnifier's zoom level and the one its toggle turns on at.
    pub(crate) magnifier: (u8, u8),
    /// Setup, while it runs: it withholds the desktop's actions.
    #[cfg(target_os = "linux")]
    pub(crate) setup: Option<Rc<RefCell<crate::setup::Setup>>>,
    /// The login screens' comforts, kept equal to the person's own.
    #[cfg(target_os = "linux")]
    pub(crate) comforts: Option<crate::comforts::Mirror>,
    /// Checking the passwords the lock screen was given.
    #[cfg(target_os = "linux")]
    pub(crate) unlocking: crate::platform::session::Unlocking,
    /// How many times the session started the lock screen's app for the
    /// lock in force: a screen that keeps going is not staying up at all,
    /// and the session draws the lock itself from then on.
    #[cfg(target_os = "linux")]
    pub(crate) lock_starts: u32,
    /// The lock drawn in this process, for the locks the lock screen's app
    /// cannot show.
    #[cfg(target_os = "linux")]
    pub(crate) lock_view: Option<crate::lock_screen::LockView>,
    /// What shows the lock in force: the lock screen's app, or, when that app
    /// cannot show it, [`Session::lock_view`] — nothing but this process then
    /// draws and unlocks it.
    #[cfg(target_os = "linux")]
    pub(crate) lock_shown: crate::lock_screen::LockShown,
    /// The login's lock record: the state this session ends in is published
    /// to it, and a session that ends without doing so comes back locked.
    #[cfg(target_os = "linux")]
    pub(crate) recovery: Option<crate::recovery::Recovery>,
    /// The clock on finishing the staged system update.
    #[cfg(target_os = "linux")]
    pub(crate) update_finish: crate::update_finish::Finish,
    /// The screenshots asked for.
    #[cfg(target_os = "linux")]
    pub(crate) captures: Option<crate::capture::Captures>,
    /// The one worker that encodes a recording and writes every capture's
    /// file, off the loop.
    #[cfg(target_os = "linux")]
    pub(crate) capture_io: Rc<crate::capture_io::CaptureIo>,
    /// The capture studio the session shows, and answers the asks of.
    #[cfg(target_os = "linux")]
    pub(crate) capture_studio: Option<crate::capture_studio::CaptureStudio>,
    /// The windows opening and closing.
    #[cfg(target_os = "linux")]
    pub(crate) windows: crate::window_motion::WindowMotions,
    /// The quick settings panel coming and going, which the compositor draws.
    #[cfg(target_os = "linux")]
    pub(crate) quick_settings_panel: crate::quick_settings::PanelMotion,
    /// The temperature and colour every app window is told, as the settings
    /// and wallpapers last said them.
    #[cfg(target_os = "linux")]
    pub(crate) warmth: crate::window_warmth::Windows,
    /// The language the session's own surfaces speak, as the person's
    /// settings last said it.
    #[cfg(target_os = "linux")]
    pub(crate) language: crate::i18n::Language,
    /// The person's warmth and when it shows, and the panel following the
    /// light in the room while they asked for it.
    #[cfg(target_os = "linux")]
    pub(crate) night_light: crate::night_light::NightLight,
    #[cfg(target_os = "linux")]
    pub(crate) auto_brightness: crate::ambient_brightness::AutoBrightness,
    /// Where each display stands and where the person types, as the readout
    /// answers with them.
    #[cfg(target_os = "linux")]
    pub(crate) readout_place: crate::panels::ReadoutPlaces,
    /// The readout itself, so a theme the person changed reaches it.
    #[cfg(target_os = "linux")]
    pub(crate) readout: Rc<RefCell<crate::panels::Readout>>,
    /// When the on-screen keyboard runs, as the person chose: never, over a
    /// field they touch, or always.
    #[cfg(target_os = "linux")]
    pub(crate) keyboard_preference: tilo_accessibility_settings::OnScreenKeyboard,
    /// Where the keyboard's app stands, while the person may want it: a
    /// keyboard on another display, or on one that changed size, is started
    /// again where they type.
    #[cfg(target_os = "linux")]
    pub(crate) keyboard_run: crate::keyboard::Run,
    /// How often the keyboard's app was started on the display it stands on,
    /// so one that keeps exiting is given up on rather than started for ever.
    #[cfg(target_os = "linux")]
    pub(crate) keyboard_starts: u32,
    /// Where quick settings' app stands, and how often it was started at
    /// that place: one that keeps exiting is given up on.
    #[cfg(target_os = "linux")]
    pub(crate) quick_settings_run: crate::quick_settings::Run,
    #[cfg(target_os = "linux")]
    pub(crate) quick_settings_starts: u32,
    /// The portal's link to the loop, while the portal serves.
    #[cfg(target_os = "linux")]
    pub(crate) portal: Option<crate::portal_link::SessionLink>,
    /// The apps this session started, told to the portal: which of them run
    /// in one of the launcher's sandboxes.
    #[cfg(target_os = "linux")]
    pub(crate) launches: std::sync::Arc<crate::launches::Launches>,
    /// The plane the apps it runs read and invoke its services on, while it
    /// serves one.
    #[cfg(target_os = "linux")]
    pub(crate) action_plane: Option<crate::plane::Plane>,
    /// The content its workspaces' records reference, and what a window it
    /// does not run is pictured into: the address is read by any caller of
    /// the action protocol.
    #[cfg(target_os = "linux")]
    pub(crate) content: std::sync::Arc<crate::content::Plane>,
    /// The control door `tilo-control` drives this session over: the
    /// development and the device admin's sockets, when the launcher names
    /// them, and the one a program inside an app reaches.
    #[cfg(target_os = "linux")]
    pub(crate) control: crate::test_control::Control,
    /// The liveness door beside the development control socket: whether the
    /// loop still turns, and where its threads are when it does not. Kept for
    /// as long as the session runs: dropping it takes the door down.
    #[allow(
        dead_code,
        reason = "held for the session's life: dropping it takes the door down"
    )]
    #[cfg(target_os = "linux")]
    pub(crate) liveness: Option<tilo_liveness::Server>,
    /// The shortcuts the person saved, started by their triggers.
    #[cfg(target_os = "linux")]
    pub(crate) automation: crate::automation::Automation,
    /// The runtime the shortcuts' runs execute on, which the apps' broker
    /// starts them on. Kept for the session's life: dropping it stops the
    /// record feed and the runs still executing on it.
    #[cfg(target_os = "linux")]
    pub(crate) automation_runtime: tokio::runtime::Runtime,
    /// The desktop services the portal and the notifications daemon run
    /// in, fed what the session knows of itself.
    #[cfg(target_os = "linux")]
    pub(crate) desktop: Option<Rc<crate::desktop_services::DesktopServices>>,
    /// The apps the portal was last told run, and how.
    #[cfg(target_os = "linux")]
    pub(crate) running: Vec<(String, u32)>,
    /// Whether the screens were locked when the portal was last told.
    #[cfg(target_os = "linux")]
    pub(crate) screen_locked: bool,
    /// The app a process runs, for the windows it shows.
    #[cfg(target_os = "linux")]
    pub(crate) pids: std::collections::BTreeMap<u32, String>,
    /// The serial of the icon the desk was last handed each window's own
    /// artwork at, and which windows it was told of at all, so a window that
    /// sets another icon is handed the new one and a window that closed lets
    /// go of its own.
    #[cfg(target_os = "linux")]
    pub(crate) window_artwork: std::collections::BTreeMap<u64, u64>,
    /// The watch that keeps the notification policy current; dropping it
    /// stops the news.
    #[cfg(target_os = "linux")]
    pub(crate) _settings_watch: Option<settings_store::watch::SettingsWatch>,
    /// The surface apps the session started, and those still running.
    #[cfg(target_os = "linux")]
    pub(crate) surfaces: crate::surfaces::Surfaces,
    /// The surface apps the session starts itself, once its outputs are
    /// there: the bar and what else the desktop runs as a process of its
    /// own.
    #[cfg(target_os = "linux")]
    pub(crate) surface_apps: Vec<crate::surfaces::Surface>,
    /// The views the person bound to the shell's slots, and the one running
    /// each slot now.
    #[cfg(target_os = "linux")]
    pub(crate) slots: crate::slots::Slots,
    /// The slots whose view is a tilolang package, which the session runs
    /// itself.
    #[cfg(target_os = "linux")]
    pub(crate) slot_views: crate::slot_views::SlotViews,
    /// The displays as the person arranged them, kept in step with the ones
    /// attached.
    #[cfg(target_os = "linux")]
    pub(crate) displays: crate::display_settings::Displays,
    /// The displays as a published service: the arrangements they are used in
    /// and how bright each one is, kept told of the outputs the compositor
    /// shows.
    #[cfg(target_os = "linux")]
    pub(crate) displays_service: crate::displays::Displays,
    /// Settings' Identify answers: which screen is which, said on the
    /// screens.
    #[cfg(target_os = "linux")]
    pub(crate) numbers: crate::display_identify::Numbers,
    /// The person's screens across their devices, as this session offers its
    /// own and publishes what every device shows.
    #[cfg(target_os = "linux")]
    pub(crate) published_displays: crate::shared_displays::Publisher,
    /// The question about a display change, and the answers it takes.
    #[cfg(target_os = "linux")]
    pub(crate) confirmation: crate::display_confirm::Confirmation,
    /// The display sheet: where the desktop appears, and the wireless
    /// displays it streams to.
    #[cfg(target_os = "linux")]
    pub(crate) display_sheet: crate::display_sheet::DisplaySheet,
    /// The frames a live projection is sent.
    #[cfg(target_os = "linux")]
    pub(crate) projection_video: crate::projection_video::ProjectionVideo,
    /// Which way up the device is held, and the lock that pins the panel.
    #[cfg(target_os = "linux")]
    pub(crate) rotation: crate::rotation::Rotation,
    /// The keyboard input sources, and the one that types.
    #[cfg(target_os = "linux")]
    pub(crate) sources: crate::config::Sources,
    /// The composing input method of the active source, while one runs.
    #[cfg(target_os = "linux")]
    pub(crate) input_method: crate::input_method::InputMethod,
    /// The machine's power: the idle policy, the service over logind and
    /// what holds it awake.
    #[cfg(target_os = "linux")]
    pub(crate) machine: crate::power::PowerRun,
    /// The shell's bar, when its package started.
    #[cfg(target_os = "linux")]
    pub(crate) shell: Option<crate::shell::Shell>,
    /// What assistive technology reads of the session's own surfaces: the
    /// desk, the bar, the switcher, the panels and the sheets, each a window
    /// of the session's AT-SPI application.
    #[cfg(target_os = "linux")]
    pub(crate) a11y: crate::a11y::Accessibility,
    /// The shell-ui socket the apps ask about their own tabs on, while it
    /// serves.
    #[cfg(target_os = "linux")]
    pub(crate) tabs: Option<crate::app_tabs::AppTabs>,
    /// The controllers in the room, and how they drive the desktop; `None`
    /// when this session reads none.
    #[cfg(target_os = "linux")]
    pub(crate) controllers: Option<crate::controllers::Controllers>,
    #[cfg(target_os = "linux")]
    pub(crate) connectors: crate::connectors::ConnectorHost,
    /// The controllers' descriptors, polled by the loop so their input ends
    /// its wait, by the key the controller source gave each.
    #[cfg(target_os = "linux")]
    pub(crate) controller_wakes: std::collections::BTreeMap<u64, tilo_compositor::ReadableWatch>,
}
