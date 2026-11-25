//! The session's state between frames, as every host runs it: what its roles keep — the session and
//! the layout it holds, the records, the people, what search reads, the notices and the
//! workspaces' handles — beside the effects a host provides, which only its own host names: the
//! compositor and the services of a Linux login (`platform::session`), the page's own on the web.
//!
//! A role in this crate takes `&Shared` or `&mut Session` and reaches an effect only through a
//! field that every host has; what only one host can do stays behind `cfg(target_os = "linux")`
//! here until its concern has a service in [`crate::platform`].

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Instant;

use tilo_session_core::desktop::host::chrome::ChromeIntent;
use tilo_session_core::desktop::host::contract::DesktopAsk;
use tilo_session_core::desktop::overlays::Overlays;
use tilo_session_core::desktop::state::DesktopHandle;

/// What the shell's send keys asked of the window in front: the box it opens,
/// and whom the box starts addressed to.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BoxAsk {
    /// Ask an agent about what the window has picked: a box addressed to the
    /// new agent it makes.
    Agent,
    /// Say something to the people here: a box addressed to them.
    People,
    /// Report a problem: the window lights what is under the pointer first, and
    /// the box opens about what the person points at, to the one place this
    /// computer sends reports.
    Report,
}

/// Where each published part stood when search last read it: the part's
/// place in the published side and its revision ([`Surfaces::revision`]).
///
/// [`Surfaces::revision`]: tilo_session_core::surfaces::Surfaces::revision
pub(crate) type Observed = RefCell<Vec<(usize, u64)>>;

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
    pub(crate) desktop: crate::platform::surfaces::ShellDesktop,
    /// The workspace's activity: the session's changes, and the records its
    /// apps keep.
    #[cfg(target_os = "linux")]
    pub(crate) activity: crate::activity::Activity,
    /// Who sits here, as the session's rules and the workspaces' governance
    /// name them.
    pub(crate) person: String,
    /// The ids they went by before this device had an identity, which their
    /// own workspaces may still name them by.
    pub(crate) earlier: Vec<String>,
    /// The device's records, where the session keeps what outlives it: what ends the session
    /// waits for it to be written.
    pub(crate) records: Option<tilo_session_client::device_records::DeviceRecords>,
    /// The content its workspaces' records reference: the device's daemon keeps it on the one
    /// shelf its records' content lives on, and this session reads and keeps it there by the
    /// address any caller of the action protocol names.
    #[cfg(target_os = "linux")]
    pub(crate) content: std::sync::Arc<crate::record_content::RecordContent>,
    #[cfg(target_os = "linux")]
    pub(crate) search: Rc<crate::search::SessionSearch>,
    /// The apps the person opens, when this session starts them.
    #[cfg(target_os = "linux")]
    pub(crate) launch: Option<Rc<crate::apps::Apps>>,
    /// This device's app lists, as last read: what the session offers a card for, and what opening
    /// one of its verbs finds.
    #[cfg(target_os = "linux")]
    pub(crate) lists: crate::lists::Lists,
    /// The windows this device left open, asked for again one at a time into
    /// the tabs they were in.
    #[cfg(target_os = "linux")]
    pub(crate) restore: RefCell<crate::restore::Restore>,
    /// The drives the person plugs in, when this session watches them.
    #[cfg(target_os = "linux")]
    pub(crate) drives: Option<Rc<crate::drives::Drives>>,
    /// The keyboards, pointers and drawing tablets attached to this machine, as a worker read
    /// them: what serves `session:input-devices`, and which keyboard typed last.
    #[cfg(target_os = "linux")]
    pub(crate) inputs: crate::input_devices::Inputs,
    /// What the session and the chrome asked of the session.
    pub(crate) asks: Rc<RefCell<Vec<DesktopAsk>>>,
    /// What the shell's send keys asked for, waiting for the pass that asks
    /// the window in front to open its box. A key can arrive while the session is
    /// between two of its own frames, so the window it was over is named then,
    /// by the session, and never at the keypress.
    #[cfg(target_os = "linux")]
    pub(crate) box_ask: Cell<Option<BoxAsk>>,
    #[cfg(target_os = "linux")]
    pub(crate) feedback: Option<crate::feedback::Destination>,
    /// The files the session handed a box to carry, until the person sends them.
    #[cfg(target_os = "linux")]
    pub(crate) box_files: crate::carried::Carried,
    /// Where the chrome takes intents the session raises.
    pub(crate) intents: Rc<RefCell<Vec<ChromeIntent>>>,
    /// What stands over the windows: its menus, quick settings, the picker
    /// and the rest, and which of them holds the keyboard.
    pub(crate) overlays: Overlays,
    /// The two stacks the surfaces stand in, as one: what the shell's views
    /// and the session's own popovers open through, so one of them closes the
    /// others.
    #[cfg(target_os = "linux")]
    pub(crate) surfaces: crate::device_lists::Surfaces,
    /// Every hosted surface's and service's published side: the session's
    /// action host.
    pub(crate) published: tilo_session_core::surfaces::Surfaces,
    /// The windows apps draw in processes of their own, as their processes sent
    /// them for the session's AT-SPI export, and the actions clients asked of
    /// them: written by the shell-ui service, read by the loop.
    #[cfg(target_os = "linux")]
    pub(crate) windows: Rc<RefCell<crate::a11y::WindowExports>>,
    /// Published actions keys and gestures asked for, waiting for the API.
    pub(crate) invocations: crate::keys::Invocations,
    /// Chords pressed that run a saved shortcut, waiting for the automation
    /// host.
    pub(crate) triggered: crate::keys::PressedChords,
    /// Super alone, as the compositor reads it: the search a tap opens and
    /// the keys a hold shows, which the pass answers.
    pub(crate) super_tap: Rc<RefCell<tilo_session_core::super_tap::SuperTap>>,
    /// The keys a hold shows, which the chrome draws over the shell.
    pub(crate) super_peek: tilo_session_core::desktop::super_peek::SuperPeek,
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
    /// cluster, the lock screen and the session read it.
    pub(crate) readings: crate::status::Shared,
    /// The hardware keys' readout: what the last press said, and when it goes,
    /// published as `device.osd` for the shell that draws it.
    #[cfg(target_os = "linux")]
    pub(crate) osd: crate::shell::osd::Shared,
    /// How far the fingers carrying the strip shown have travelled since they landed: what the
    /// shell's row was scrolled by already.
    #[cfg(target_os = "linux")]
    pub(crate) carried: std::cell::Cell<f64>,
    /// The system services, reached for their readings.
    pub(crate) services: Rc<RefCell<tilo_service::Services>>,
    /// What the device's dispatch asks of those services: a press of one of their actions, and
    /// the person's answer to the question it asked.
    pub(crate) served_services: std::sync::Arc<crate::services::app_wire::ServedServices>,
    /// The sends the session's send action took, which the pass carries out.
    #[cfg(target_os = "linux")]
    pub(crate) sends: crate::send_to::Sends,
    /// The resources a Nearby Send action asked to share, which the pass opens the invitation
    /// sheet for.
    #[cfg(target_os = "linux")]
    pub(crate) nearby_shares: Rc<RefCell<Vec<String>>>,
    /// The session's Nearby Send: what a send asks of the seat's shell is answered on the pass.
    #[cfg(target_os = "linux")]
    pub(crate) nearby: crate::nearby_send::Served,
    /// The services the session serves its apps itself.
    #[cfg(target_os = "linux")]
    pub(crate) table: crate::services::Served,
    /// The windows this session shares with its workspaces, and the streams it receives from them.
    #[cfg(target_os = "linux")]
    pub(crate) window_shares: Rc<RefCell<crate::window_share::WindowShares>>,
    /// The windows whose apps hand over their page as a paint stream: what
    /// this session publishes on the workspace's live session while someone
    /// shows them.
    #[cfg(target_os = "linux")]
    pub(crate) pages: Rc<RefCell<crate::page_track::PageTracks>>,
    /// The agents each workspace keeps, read every pass from its record of
    /// them: who keeps an agent is that record's word, and what the broker
    /// admits an agent's session on.
    #[cfg(target_os = "linux")]
    pub(crate) agents: std::rc::Rc<crate::agents::Agents>,
    /// The device's daemon on the session's shell link: where the workspace resources the session
    /// reads itself are followed, and their typed actions run.
    #[cfg(target_os = "linux")]
    pub(crate) daemon: tilo_session_client::daemon_documents::DaemonShell,
    /// The agents the person admitted at its MCP server, as the session and the bar show them.
    #[cfg(target_os = "linux")]
    pub(crate) agent_sessions: std::sync::Arc<crate::agent_sessions::AgentSessions>,
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
    pub(crate) handoff: Rc<std::cell::RefCell<Option<crate::hosted::MediaOffer>>>,
    /// The wakes this session takes part in.
    #[cfg(target_os = "linux")]
    pub(crate) waking: Rc<crate::wake::Wake>,
    /// This device's share of the person's one keyboard and pointer, while it is on their fabric.
    #[cfg(target_os = "linux")]
    pub(crate) keyboard_and_pointer: Option<std::sync::Arc<crate::shared_input::SharedInput>>,
    pub(crate) wake_handle: tilo_service::Wake,
    /// Workspace settings, the questions the session asks for apps and
    /// services, and the one deleting a workspace asks, by their mounts.
    #[cfg(target_os = "linux")]
    pub(crate) workspace_settings: Option<tilo_sheets::workspace_settings::WorkspaceSettingsState>,
    /// The shell's revision and the workspace state's, as workspace settings
    /// was last fed at: its panes are rebuilt when either changed under it.
    pub(crate) ws_settings_fed: Cell<Option<(u64, u64)>>,
    /// The session revision the file access list and what apps may capture were
    /// last worked out at: both follow every change of the session, so a
    /// revocation reaches the running app at once.
    pub(crate) capabilities_fed: Cell<Option<(u64, u64, u64)>>,
    /// The workspace's agents and emoji, which workspace settings shows and
    /// changes through Chat's record of the workspace.
    #[cfg(target_os = "linux")]
    pub(crate) workspace_tabs: crate::workspace_settings::WorkspaceTabs,
    /// The question about a display change, which stands on every display
    /// at once so it is seen even when the change blanked one of them.
    #[cfg(target_os = "linux")]
    pub(crate) display_confirm: Option<tilo_sheets::display_confirm::ConfirmState>,
    /// The workspaces this computer may join or open.
    #[cfg(target_os = "linux")]
    pub(crate) directory: Option<tilo_sheets::directory::DirectoryState>,
    /// Inviting people: the sheet a link into the workspace in front is made
    /// from, and what it made in this opening.
    pub(crate) invite: Option<tilo_sheets::invite::InviteState>,
    pub(crate) inviting: crate::invite::Invite,
    /// The question deleting a workspace asks, and the workspace it is about.
    #[cfg(target_os = "linux")]
    pub(crate) delete_workspace: Option<tilo_sheets::delete_workspace::DeleteWorkspaceState>,
    pub(crate) delete_workspace_ask: RefCell<Option<tilo_desktop_model::WorkspaceId>>,
    /// The deletions this device's daemon was asked for, by workspace and name, until it answers.
    pub(crate) deletions: RefCell<
        Vec<(
            tilo_desktop_model::WorkspaceId,
            String,
            crate::engine_write::Polled<Result<(), String>>,
        )>,
    >,
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
    /// The record of the person's devices, where this device reads how their session stands on the
    /// others and says its own; none where this device has no identity.
    #[cfg(target_os = "linux")]
    pub(crate) owner_devices: Option<crate::owner_session::OwnerDevices>,
    /// The desktop's own notices: what the session tells the person, such as
    /// leaving a workspace.
    pub(crate) notices: tilo_session_core::desktop::state::DesktopNotifications,
    /// The windows carried to another workspace, asked about first.
    #[cfg(target_os = "linux")]
    pub(crate) moves: crate::window_move::Moves,
    /// The workspaces being branched, put on the shell once what they hold is copied.
    #[cfg(target_os = "linux")]
    pub(crate) branches: crate::workspace_branch::Branches,
    /// The apps' live activities on the bar, which the pill names and the
    /// controls pressed on it go back to the app through.
    #[cfg(target_os = "linux")]
    pub(crate) activities: Rc<crate::shell::activities::Activities>,
    /// What is live now (`session:live`), which the media keys and the lock screen's transport
    /// act on before the player here.
    #[cfg(target_os = "linux")]
    pub(crate) live: Rc<crate::live_list::Sources>,
    /// The pictures the session shows, by workspace.
    #[cfg(target_os = "linux")]
    pub(crate) wallpapers: std::sync::Arc<crate::wallpapers::Wallpapers>,
    /// The picture the workspace in front shows, as the views' host draws it under everything.
    pub(crate) wallpaper: ui::backdrop::wallpaper::Source,
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
    /// What those questions tell the person without asking.
    #[cfg(target_os = "linux")]
    pub(crate) program_notices: crate::program_asks::ProgramNotices,
    /// Where programs ask the person: what the session lends its apps and asks itself on.
    #[cfg(target_os = "linux")]
    pub(crate) asker: tilo_service::AskSender,
    /// Who is here: this session's membership of each workspace, when it
    /// takes part in the peer network.
    pub(crate) sync: Option<Rc<crate::sync::Client>>,
    /// How the person's workspaces stand, as the device's daemon publishes them, and the actions
    /// that change them.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) view: Option<std::sync::Arc<crate::workspace_view::WorkspaceView>>,
    /// What became of the invitations the device's daemon was asked to join, since the loop last
    /// took them.
    #[cfg(target_os = "linux")]
    pub(crate) joins: Rc<RefCell<Vec<tilo_session_client::Enrolled>>>,
    /// The documents of the workspaces this device's service holds, as the session's own roles
    /// read them.
    #[cfg(target_os = "linux")]
    pub(crate) document_reads:
        std::sync::Arc<dyn tilo_peer_state::workspace_documents::DeviceDocuments>,
    /// The store of a workspace this device's own service holds, once it is open: where the apps
    /// the session runs itself keep their resources, as the apps' windows do.
    #[cfg(target_os = "linux")]
    pub(crate) held_stores: crate::app_stores::HeldStores,
    /// The workspaces the device's daemon holds, as the session reads and writes a record of theirs
    /// itself: the record a window carries to another workspace. `None` until it holds them.
    #[cfg(target_os = "linux")]
    pub(crate) device_workspaces: RefCell<Option<crate::device_workspaces::DeviceWorkspaces>>,
    /// Where moving the apps' resources into the workspace stands.
    #[cfg(target_os = "linux")]
    pub(crate) app_stores: RefCell<crate::app_stores::AppStores>,
    /// The workspaces' shared desktops, as their records hold them: what each
    /// device that shows a workspace puts on its own session, and what this one
    /// writes back.
    #[cfg(target_os = "linux")]
    pub(crate) desktops: Rc<crate::layout::Layouts>,
    /// What each window this session runs shows, read from the broker every
    /// pass: the session's reading of it, and the record noted beside the window
    /// in its workspace's layout.
    pub(crate) notes: std::sync::Arc<crate::continuations::Continuations>,
    /// The records the device's broker says each window's app holds, by window instance, as the
    /// session follows them on its shell link ([`crate::live_list::follow_window_notes`]): what a
    /// window's note names. A cache of the daemon's reading, never state of this session's.
    pub(crate) window_routes: crate::live_list::WindowRoutes,
    /// The engine revision, and the workspaces the session showed, the session's
    /// copy of each workspace's people was last taken from: what nothing new
    /// of a workspace's governance, and no workspace the session did not show
    /// before, has passed since.
    pub(crate) membership_fed:
        std::cell::RefCell<Option<(u64, Vec<tilo_desktop_model::WorkspaceId>)>>,
    /// What the loop does with the answers of async work it started (a signature, a commit into a
    /// workspace's record), on the pass after each landed.
    pub(crate) landings: Rc<crate::landing::Landings>,
    /// The person's own workspaces, as all their devices hold the list of
    /// them: `None` when this computer has no identity to keep it for.
    #[cfg(target_os = "linux")]
    pub(crate) personal: Option<RefCell<crate::workspaces::Personal>>,
    /// The copies the person can paste again.
    #[cfg(target_os = "linux")]
    pub(crate) clipboard: Rc<crate::clipboard::Clipboard>,
    /// The person's layout: the session this session holds, with history and
    /// undo (shell.md §2), and the app places its windows from it.
    pub(crate) layout: Rc<RefCell<tilo_session_core::layout::Layout>>,
    /// Whether the person is mid-gesture or typing, as the compositor last
    /// said: what a layout action that is not their own waits for.
    pub(crate) underway: Rc<Cell<tilo_session_core::layout::Busy>>,
    /// When the person last typed text, as the compositor last said: what `device.input`
    /// publishes.
    pub(crate) typed_at: Rc<Cell<Option<Instant>>>,
    /// The person's and the device's settings, kept as resources of their workspaces.
    #[cfg(target_os = "linux")]
    pub(crate) settings: crate::session_settings::SessionSettings,
    /// Whether the overview is the surface up: the shell's surfaces turn it as they open and close
    /// it, and the control socket's state reads it.
    pub(crate) overview: Rc<Cell<bool>>,
    /// Dictation: the speech the person allows once per dictation, and the
    /// sheet they answer on.
    #[cfg(target_os = "linux")]
    pub(crate) dictation: crate::dictation::Dictation,
    /// The emoji picker's picks: written into the field the person was typing
    /// into, and where that field's caret stands for the card to open at.
    #[cfg(target_os = "linux")]
    pub(crate) emoji: crate::emoji::Emoji,
    /// The shortcut guide: the sheet naming every key the desktop answers,
    /// and the table it explains.
    #[cfg(target_os = "linux")]
    pub(crate) shortcut_guide: crate::shell::shortcut_guide::ShortcutGuide,
    /// Who is here, as the bar's faces open it: the roster it shows and
    /// where everyone else's pointer is.
    #[cfg(target_os = "linux")]
    pub(crate) people: Rc<RefCell<crate::people::People>>,
    /// What the roster's own resource asked of the session — a follow, a wave — carried out on the
    /// next pass beside what a card asked.
    #[cfg(target_os = "linux")]
    pub(crate) people_asks: Rc<RefCell<Vec<crate::people::PeopleAsk>>>,
    /// Whose view this device follows — its own presence word — and which of their place it
    /// already brought forward.
    pub(crate) follow: RefCell<tilo_session_core::presence::Follow>,
    /// Whether the person may invite someone into the workspace in front, as
    /// the loop last answered it: the bar's invite button and the roster's
    /// footer are offered on it.
    pub(crate) invitable: Rc<Cell<bool>>,
    /// The controller the person last drove the desktop with: the guide
    /// leads with its buttons until a key of their own is pressed.
    pub(crate) controller: Rc<Cell<Option<ui::ControllerFamily>>>,
    /// Whether the keys being routed now are a controller's own,
    /// for its presses: those say nothing about a keyboard.
    pub(crate) controller_keys: Rc<Cell<bool>>,
    /// The questions about the person's apps: whether to add a program they
    /// downloaded to their apps, and whether to uninstall one.
    #[cfg(target_os = "linux")]
    pub(crate) app_sheets: crate::app_sheets::AppSheets,
    /// The MCP server, when this session serves one: what a program of this
    /// login asks on, and the person's answers about it.
    #[cfg(target_os = "linux")]
    pub(crate) mcp_server: RefCell<Option<crate::mcp_server::Loop>>,
    /// What a connection of any API asks of the session's published side: answered
    /// on the loop's pass, the only place that may touch them.
    #[cfg(target_os = "linux")]
    pub(crate) session_services: std::sync::Arc<crate::mcp_server::SessionServices>,
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
    pub(crate) active_workspace: Option<tilo_desktop_model::WorkspaceId>,
    /// The workspace in front as the login's own programs read it, which
    /// they join ([`crate::published_workspace`]).
    #[cfg(target_os = "linux")]
    pub(crate) published_workspace: crate::published_workspace::Published,
    /// The session's own actions, as the broker's steps: what each pass hands the rows it read.
    #[cfg(target_os = "linux")]
    pub(crate) published_steps: Option<std::sync::Arc<crate::session_actions::SessionActions>>,
    /// The person's settings, as the broker's steps of Settings' own provider: what each pass
    /// hands the settings the session's service published.
    #[cfg(target_os = "linux")]
    pub(crate) settings_steps: Option<std::sync::Arc<crate::settings_steps::SettingsSteps>>,
    /// When the session's own bookkeeping last read it.
    pub(crate) kept_at: Instant,
    /// What the session's input a tool handed the loop cost this pass, read with
    /// the control socket's own time in the frame's timings.
    pub(crate) tool_input: std::time::Duration,
    /// The magnifier's zoom level and the one its toggle turns on at.
    pub(crate) magnifier: (u8, u8),
    /// Setup, while it runs: it withholds the desktop's actions.
    #[cfg(target_os = "linux")]
    pub(crate) setup: Option<Rc<RefCell<crate::setup::Setup>>>,
    /// The identity backup a desktop already in use opens, as setup shows it when a computer is
    /// first used ([`crate::Options::identity_backup`]): a layer raised over the desktop.
    #[cfg(target_os = "linux")]
    pub(crate) identity_backup:
        fn(bool) -> Result<tilo_onboarding_session::view::OnboardingView, String>,
    /// Whether this session is Tilo installed as a desktop session on an existing Linux host.
    #[cfg(target_os = "linux")]
    pub(crate) host_desktop: bool,
    /// Physical pixels per logical pixel the desktop draws at, for a layer the session raises
    /// after its first frame.
    #[cfg(target_os = "linux")]
    pub(crate) scale: f64,
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
    /// How the session ends ([`crate::Options::teardown`]).
    #[cfg(target_os = "linux")]
    pub(crate) teardown: Box<dyn Fn() -> tilo_app_launch::Teardown>,
    /// The login's record that this desktop is usable, published once every
    /// output draws and the windows are being restored; it is removed with
    /// the session that wrote it.
    #[cfg(target_os = "linux")]
    pub(crate) ready: Option<crate::ready::Desktop>,
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
    /// The temperature and colour every app window is told, as the settings
    /// and wallpapers last said them.
    #[cfg(target_os = "linux")]
    pub(crate) warmth: crate::window_warmth::Windows,
    /// The language the session's own surfaces speak, as the person's
    /// settings last said it.
    #[cfg(target_os = "linux")]
    pub(crate) language: crate::i18n::Language,
    /// The person's region formats, as the session's own surfaces and the
    /// tilolang hosts it runs last read them.
    #[cfg(target_os = "linux")]
    pub(crate) formats: crate::formats::Formats,
    /// The person's warmth and when it shows, and the panel following the
    /// light in the room while they asked for it.
    #[cfg(target_os = "linux")]
    pub(crate) night_light: crate::night_light::NightLight,
    #[cfg(target_os = "linux")]
    pub(crate) auto_brightness: crate::ambient_brightness::AutoBrightness,
    /// Where the person types, as the compositor last said: the caret the surfaces a key opens
    /// stand beside.
    #[cfg(target_os = "linux")]
    pub(crate) caret: Cell<Option<ui::kurbo::Rect>>,
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
    /// The portal's link to the loop, while the portal serves.
    #[cfg(target_os = "linux")]
    pub(crate) portal: Option<crate::portal_link::SessionLink>,
    /// The apps this session started, told to the portal: which of them run
    /// in one of the launcher's sandboxes.
    #[cfg(target_os = "linux")]
    pub(crate) launches: std::sync::Arc<crate::launches::Launches>,
    /// The app wire the apps it runs read and invoke its services on, while it
    /// serves one.
    #[cfg(target_os = "linux")]
    pub(crate) app_wire: Option<crate::app_wire::AppWire>,
    /// The content its workspaces' records reference, and what a window it
    /// does not run is pictured into: the device's daemon keeps it on the one
    /// shelf its records' content lives on, and this session reads and keeps
    /// it there by the address any caller of the action protocol names.
    #[cfg(target_os = "linux")]
    pub(crate) content: std::sync::Arc<crate::record_content::RecordContent>,
    /// The control socket `tilo-control` drives this session over: the
    /// development and the device admin's sockets, when the launcher names
    /// them, and the one a program inside an app reaches.
    #[cfg(target_os = "linux")]
    pub(crate) control: crate::test_control::Control,
    /// The liveness socket beside the development control socket: whether the
    /// loop still turns, and where its threads are when it does not. Kept for
    /// as long as the session runs: dropping it takes the socket down.
    #[allow(
        dead_code,
        reason = "held for the session's life: dropping it takes the socket down"
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
    /// The key of the artwork the session was last handed for each window, and
    /// which windows it was told of at all, so a window that sets another
    /// icon is handed the new one and a window that closed lets go of its own.
    #[cfg(target_os = "linux")]
    pub(crate) window_artwork: std::collections::BTreeMap<u64, String>,
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
    /// The views the person bound to the shell's parts, and the one running
    /// each part now.
    #[cfg(target_os = "linux")]
    pub(crate) parts: crate::parts::Parts,
    /// The parts whose view is a tilolang package, which the session runs
    /// itself.
    #[cfg(target_os = "linux")]
    pub(crate) part_views: crate::part_views::PartViews,
    /// The displays as the person arranged them, kept in step with the ones
    /// attached.
    #[cfg(target_os = "linux")]
    pub(crate) displays: crate::display_settings::Displays,
    /// The displays as a published service: the modes they are used in
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
    /// What this device holds of its workspaces, kept in the record of the
    /// person's devices so their other devices read it.
    #[cfg(target_os = "linux")]
    pub(crate) published_storage: crate::shared_storage::Publisher,
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
    /// session, the bar, the switcher, the panels and the sheets, each a window
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
    /// The apps' outside services the session's own services reach, through the connectors the
    /// device's daemon runs.
    #[cfg(target_os = "linux")]
    pub(crate) origins: crate::connectors::Reached,
    /// The controllers' descriptors, polled by the loop so their input ends
    /// its wait, by the key the controller source gave each.
    #[cfg(target_os = "linux")]
    pub(crate) controller_wakes: std::collections::BTreeMap<u64, tilo_compositor::ReadableWatch>,
    /// The test's daemon this session is the shell of ([`crate::Options::shell_daemon`]): the
    /// last field, so it ends after everything of the session that reaches it.
    #[cfg(unix)]
    pub(crate) shell_daemon: Option<Box<dyn std::any::Any>>,
}
