//! This device's lists as resources of the session's: its windows, the tray, the agents at the
//! person's peer network and the styles, and inviting into a workspace. A shell's views read them, and
//! so does a script or an agent, through the API, by the same addresses: one reading for every
//! reader.

use std::rc::Rc;

use fs::tree_sync::{Refusal, Waiting};
use serde_json::{Map, Value};
use tilo_actions::remote::session_actions::INVITE_RESOURCE;
use tilo_actions::{ActionAddress, ApiError, ResourceSpec};
use tilo_desktop::chrome::ChromeIntent;
use tilo_desktop::desktop_host::DesktopAsk;
use tilo_desktop::overlays::{Overlay, Overlays, Part, SwitcherScope};
use tilo_desktop_model::{Layout, Workspace, WorkspaceId};
use tilo_desktop_surface::DesktopHandle;
use tilo_peer_state::join::Role;
use tilo_resource::Effect;
use tilo_service::{Service, Started, decode, offer, reading};
use tilo_session_core::desktop::navigation::ChromeEffect;

use crate::windows::{reaching, windows};

use tilo_action_policy::{Answer, Grantee};
use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand as Command;

/// What the device's lists are made from ([`DeviceLists::lists_key`]), one entry per resource: the
/// windows, the tray, the agents and the styles. An entry that moved is a reading that moved.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ListsKey([(u64, u64, u64, bool); 5]);

/// The shell's windows, every workspace's: what the switcher, the dock and the launcher list.
pub(crate) const WINDOWS_RESOURCE: &str = "session:windows";
/// The running applications' tray items.
pub(crate) const TRAY_RESOURCE: &str = "session:tray";
/// The agents the person admitted at their MCP server.
pub(crate) const AGENTS_RESOURCE: &str = "session:agents";
/// The styles every installed package offers.
pub(crate) const STYLES_RESOURCE: &str = "session:styles";
/// The workspaces' folders on this computer, and whether what is put in them reached the server.
pub(crate) const FOLDERS_RESOURCE: &str = "session:folders";

/// The windows as a view of another language reads them (`device.windows`).
#[must_use]
pub fn windows_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<crate::windows::Windows, tilo_resource::NoEffect>(
        "windows", "Windows",
    )
}

/// The tray as a view of another language reads it (`device.tray`).
#[must_use]
pub fn tray_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<Tray, TrayEffect>("tray", "Tray")
}

/// The agents as a view of another language reads them (`device.agents`).
#[must_use]
pub fn agents_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<crate::agent_sessions::Agents, AgentsEffect>(
        "agents", "Agents",
    )
}

/// The styles as a view of another language reads them (`device.styles`).
#[must_use]
pub fn styles_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<OfferedStyles, tilo_resource::NoEffect>(
        "styles", "Styles",
    )
}

/// The folders as a view of another language reads them (`device.folders`).
#[must_use]
pub fn folders_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<Folders, tilo_resource::NoEffect>(
        "folders", "Folders",
    )
}

/// The workspaces' folders on this computer, by workspace.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub(crate) struct Folders {
    pub folders: Vec<Folder>,
}

/// One workspace's folder, as the person is told where it stands.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub(crate) struct Folder {
    /// The workspace, by its key.
    pub workspace: String,
    /// The workspace's name, empty while it has none.
    pub name: String,
    /// The name of the device that keeps the workspace's files, empty while none is named.
    pub server: String,
    /// What the folder is doing: `paused`, `waiting-for-workspace`, `opening`, `stuck`,
    /// `sending`, `content-going-out` (every change went out, and the server still lacks some
    /// files' contents) or `in-sync`.
    pub state: String,
    /// Why changes made here wait, while they do, as the reason the person needs to hear first:
    /// `unopened` (the workspace hasn't said which files it holds), `unanswered` (it hasn't
    /// answered for some of them), `unarrived` (part of its file list hasn't arrived here),
    /// `ordered` (changes wait for others to arrive first), `refused` (the workspace refused some,
    /// for [`Self::refused`]), `unsent` (some are sent again) or `lacking` (files whose details
    /// nobody holds); empty otherwise.
    pub stuck: String,
    /// Why the workspace refused changes, while that is why they wait: `history-full` (the
    /// record's history there has no room until it is compacted), `not-permitted` (the
    /// workspace's rules don't let the person make them), `unanswered` (it didn't answer in
    /// time) or `other`; empty otherwise.
    pub refused: String,
    /// The files in the folder.
    pub files: u64,
}

impl From<tilo_workspace_service::files::Folder> for Folder {
    fn from(folder: tilo_workspace_service::files::Folder) -> Self {
        let state = serde_json::to_value(&folder.status.state)
            .ok()
            .and_then(|state| state.as_str().map(str::to_owned))
            .unwrap_or_default();
        let (stuck, refused) = match folder.status.stuck.into_iter().next() {
            Some(Waiting::Refused(reason)) => ("refused".to_owned(), refusal_kind(&reason)),
            Some(waiting) => (
                serde_json::to_value(&waiting)
                    .ok()
                    .and_then(|said| said.as_str().map(str::to_owned))
                    .unwrap_or_default(),
                String::new(),
            ),
            None => (String::new(), String::new()),
        };
        Self {
            workspace: folder.workspace,
            name: folder.name,
            server: folder.server,
            state,
            stuck,
            refused,
            files: u64::try_from(folder.status.files).unwrap_or(u64::MAX),
        }
    }
}

/// The kind of `refusal`, by the name a view words it from: the detail of any other refusal is
/// in the words of whoever refused it, which no view shows a person.
fn refusal_kind(refusal: &Refusal) -> String {
    match refusal {
        Refusal::Other(_) => "other".to_owned(),
        kind => serde_json::to_value(kind)
            .ok()
            .and_then(|said| said.as_str().map(str::to_owned))
            .unwrap_or_default(),
    }
}

/// The running applications' tray items, in the order they registered.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub(crate) struct Tray {
    pub items: Vec<TrayItem>,
}

/// A running application's tray item, as the bar shows it.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub(crate) struct TrayItem {
    /// The item's key, which a press on it names.
    pub key: String,
    /// The app's own words for it.
    pub title: String,
    /// Whether choosing it opens its menu rather than its window.
    pub menu: bool,
    /// How many unread notifications the app that owns it has, as its badge shows.
    pub badge: i64,
    /// Whether it wants the person: a pulsing dot, as a status notifier says with `NeedsAttention`.
    pub attention: bool,
}

/// The styles every installed package offers (shell.md §6).
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub(crate) struct OfferedStyles {
    pub offered: Vec<crate::styles::Style>,
}

/// What the tray offers.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.session.tray")]
pub(crate) enum TrayEffect {
    /// Open or close the running application's tray item `key`: its window, or its menu when the
    /// item is one
    #[effect(kind = "outside")]
    #[effect(row, label = "session-tray-open")]
    OpenTrayItem { key: String },
}

/// What the agents offer.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.session.agents")]
pub(crate) enum AgentsEffect {
    /// End one agent session: its connection closes and its presence leaves the bar.
    #[effect(kind = "outside")]
    #[effect(label = "session-agents-stop", audience = "people")]
    StopAgent {
        #[param("The agent session, by its key")]
        agent: String,
    },
    /// Whether this computer accepts the person's agents from their other devices, and whether
    /// their steps go unasked here: the grant this device keeps in its workspaces.
    #[effect(kind = "outside")]
    #[effect(label = "session-agents-other-devices", audience = "people")]
    OtherDeviceAgents,
    /// Accept agents from the person's other devices on this computer, or stop accepting them:
    /// the grant this device keeps in its workspaces.
    #[effect(kind = "outside")]
    #[effect(label = "session-agents-accept-other-devices", audience = "people")]
    SetAcceptOtherDeviceAgents {
        #[param("Whether this computer accepts the person's agents from their other devices")]
        on: bool,
    },
    /// Let the steps of agents from the person's other devices go unasked on this computer, or
    /// put them to the person again.
    #[effect(kind = "outside")]
    #[effect(label = "session-agents-act-without-asking", audience = "people")]
    SetOtherDeviceAgentsUnasked {
        #[param("Whether their steps go unasked on this computer")]
        on: bool,
    },
}

/// The scopes a walk of the switcher crosses, as the walk's action names them: the whole workspace
/// (Alt+Tab), or the app the window in front shows (Alt and the grave key).
pub const SWITCHER_SCOPES: [&str; 2] = ["workspace", "app"];

/// What inviting offers: the links into a workspace, the invite sheet, and adding another device or
/// an agent of the person's.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.session.invite")]
pub(crate) enum InviteEffect {
    /// The invitations this device's person made into a workspace that can still be
    /// redeemed, in the words the workspace's host lists them by
    #[effect(kind = "outside")]
    Invitations {
        /// The workspace, by its key; the one in front when absent
        #[param("The workspace, by its key; the one in front when absent")]
        workspace: Option<String>,
    },
    /// Make an invitation into a workspace from this device: the link to hand to whoever is
    /// being invited, which their device joins with
    #[effect(kind = "outside")]
    IssueInvitation {
        /// The part the invitation brings them in at: `viewer`, `commenter`, `editor`,
        /// `moderator` or `administrator`; editor when absent
        #[param(
            "The part the invitation brings them in at: `viewer`, `commenter`, `editor`, `moderator` or `administrator`"
        )]
        role: Option<String>,
        /// The name the invitation knows them by
        #[param("The name the invitation knows them by")]
        invitee_name: Option<String>,
        /// The person the link is for, by their user key in hex: only a device of theirs may redeem
        /// it. Anyone holding the link may redeem it when absent
        #[param(
            "The person the link is for, by their user key in hex; anyone holding it when absent"
        )]
        recipient: Option<String>,
        /// How long the link works for, in seconds
        #[param("How long the link works for, in seconds")]
        expires_in_seconds: Option<u64>,
        /// The workspace, by its key; the one in front when absent
        #[param("The workspace, by its key; the one in front when absent")]
        workspace: Option<String>,
    },
    /// Open the invite sheet on a workspace: where a link into it is made, the part it brings
    /// someone in at picked and the links that still work taken back. An app shares what it shows
    /// by asking this, so it offers the one sheet the bar's invite button opens.
    #[effect(screen)]
    ShowInvite {
        /// The workspace, by its key; the one in front when absent
        #[param("The workspace, by its key; the one in front when absent")]
        workspace: Option<String>,
    },
    /// Show how an agent or another computer connects as the person: the setup their own card's
    /// "Connect an agent or computer" opens. An app that lists what acts as them offers adding to
    /// it this way.
    #[effect(screen)]
    ShowConnect,
    /// Open the Add a device sheet: the link a new device of the person's becomes one of theirs,
    /// shown as a code its camera reads. An app that lists what acts as the person offers adding
    /// to it this way.
    #[effect(screen)]
    ShowPairDevice,
    /// Cancel the invitation `invite` of a workspace: its link admits nobody from now on
    #[effect(kind = "outside")]
    CancelInvitation {
        /// The invitation's id, 64 hex characters, as listing invitations reports it
        #[param("The invitation's id, 64 hex characters, as listing invitations reports it")]
        invite: String,
        /// The workspace, by its key; the one in front when absent
        #[param("The workspace, by its key; the one in front when absent")]
        workspace: Option<String>,
    },
}

/// The surfaces the shell shows over its windows, and where each stands: the shell's own views,
/// except the quick settings, which stand in the panel's own app where the device has one and
/// no shell covers the shell that app stands in.
/// What `show-surface` and `close-surface` open and close, whether the person asks on the
/// shell's surface or on the chrome's above it, so both open the one surface.
#[derive(Clone)]
pub(crate) struct Surfaces {
    /// Whether the device has the quick settings' app; without, the status opens the shell's own
    /// view of them.
    pub(crate) status_panel: bool,
    /// Whether the shell draws the whole screen over the shell: the shell draws none of the panel's
    /// app there, so the quick settings are the shell's own view, as every other surface is.
    pub(crate) shell: bool,
    /// The stack the panel's own app is placed in, with the shell's surface up.
    pub(crate) overlays: Overlays,
    /// The notifications the shell's own chrome counts and draws: where the one centre opens
    /// when the shell draws it, as the bar's bell opens it.
    pub(crate) notices: tilo_desktop_surface::DesktopNotifications,
    /// Whether the overview is the surface up, as the shell's own widgets fold behind it and as
    /// whoever asks for the shell's state reads it. It turns with the surface itself, so a person's
    /// press answers at once rather than a frame later.
    pub(crate) overview: Rc<std::cell::Cell<bool>>,
    /// The shell's desktop handle, read before opening the switcher so an empty walk never leaves
    /// the switcher up holding the keyboard.
    pub(crate) handle: DesktopHandle,
    /// Whether the emoji picker stands over the windows, as the shell's own view draws it. The
    /// shell that draws the screen owns its picker: the card is the shell's view, and the keys the
    /// person types into it are the shell's, so the session keeps where it is rather than the
    /// chrome's stack.
    pub(crate) emoji: Rc<std::cell::Cell<bool>>,
    /// Whether the About sheet stands over the windows, as the shell's own view draws it.
    pub(crate) about: Rc<std::cell::Cell<bool>>,
}

/// The name the shell's own view gives the emoji picker, in what stands over the windows.
pub(crate) const EMOJI: &str = "emoji";
/// The name the shell's own view gives the About sheet, in what stands over the windows.
pub(crate) const ABOUT: &str = "about";
/// The surface a held Super's key card makes the shell say it shows: a picture over the windows,
/// which takes no key.
pub(crate) const PEEK: &str = tilo_session_core::shell::PEEK_SURFACE;

impl Surfaces {
    /// Whether the quick settings stand in the panel's own app: the device has it, and the shell
    /// draws the app it stands in.
    pub(crate) fn panel_shows(&self, surface: &str) -> bool {
        surface == crate::quick_settings::SURFACE && self.status_panel && !self.shell
    }

    /// Whether the shell's own chrome draws the centre, rather than the shell's view of it: the
    /// shell draws it whenever it draws the screen, and the bell counts the same one.
    fn shell_centre(&self, surface: &str) -> bool {
        surface == Overlay::Centre.name() && !self.shell
    }

    /// Show `surface` (`show-surface`): the one up closes, and any other opens.
    pub(crate) fn toggle(&self, surface: &str) {
        self.caught_up();
        if surface == EMOJI {
            let opening = !self.emoji.get();
            self.close_views();
            self.emoji.set(opening);
            self.settled();
            return;
        }
        if surface == ABOUT {
            let opening = !self.about.get();
            self.close_views();
            self.about.set(opening);
            self.settled();
            return;
        }
        // Another surface opens: the shell's own views go down under it.
        self.emoji.set(false);
        self.about.set(false);
        if self.shell_centre(surface) {
            // One centre: the shell's chrome draws the one the bar's bell counts, and it is asked
            // to open or shut as the bell asks it.
            self.notices.ask_centre(!self.notices.centre_open());
            self.panel_closes();
            self.settled();
            return;
        }
        // The quick settings are their own app, where the stack places its part: a
        // press opens the part, and the panel's app draws it there. A device without that app,
        // or one the shell covers, keeps the shell's own view of the surface.
        if self.panel_shows(surface) {
            if self.overlays.is_open(Part::QuickSettings) {
                self.overlays.close(Part::QuickSettings);
            } else {
                self.clear_shell();
                self.overlays.open(Part::QuickSettings, None);
            }
            return;
        }
        if let Some(surface) = Overlay::named(surface) {
            if self.overlays.here() == surface {
                self.panel_closes();
                if surface == Overlay::Switcher {
                    self.overlays.forget_walk();
                }
                self.overlays.show(Overlay::Rest);
            } else {
                if surface == Overlay::Switcher
                    && crate::windows::switcher_walk_empty_in(
                        &self.handle,
                        SwitcherScope::Workspace,
                    )
                {
                    return;
                }
                self.panel_closes();
                self.overlays.show(surface);
            }
        } else {
            self.panel_closes();
        }
        self.clear_beside();
        self.settled();
    }

    /// Show the surface `name` names (`show-surface`), whether or not it is up.
    pub(crate) fn open(&self, name: &str) {
        self.caught_up();
        if name == EMOJI {
            self.close_views();
            self.emoji.set(true);
            self.settled();
            return;
        }
        if name == ABOUT {
            self.close_views();
            self.about.set(true);
            self.settled();
            return;
        }
        // Another surface opens: the shell's own views go down under it.
        self.emoji.set(false);
        self.about.set(false);
        if self.shell_centre(name) {
            self.notices.ask_centre(true);
            self.panel_closes();
            self.settled();
            return;
        }
        if self.panel_shows(name) {
            self.clear_shell();
            self.overlays.open(Part::QuickSettings, None);
            return;
        }
        if let Some(surface) = Overlay::named(name) {
            if surface == Overlay::Switcher
                && crate::windows::switcher_walk_empty_in(&self.handle, SwitcherScope::Workspace)
            {
                if self.overlays.here() == Overlay::Switcher {
                    self.overlays.forget_walk();
                    self.overlays.show(Overlay::Rest);
                    self.settled();
                }
                return;
            }
            self.panel_closes();
            self.overlays.show(surface);
        } else {
            self.panel_closes();
        }
        self.clear_beside();
        self.settled();
    }

    /// One surface at a time: the shell's own popovers go down under the shell's view of the surface
    /// just opened.
    fn clear_beside(&self) {
        if self.overlays.here() == Overlay::Rest {
            return;
        }
        self.clear_panel();
    }

    /// Take every popover of the shell's own down: a surface outside its stack is coming up instead.
    pub(crate) fn clear_panel(&self) {
        for part in Part::ALL {
            if part.stands_beside() {
                self.overlays.close(part);
            }
        }
    }

    /// One surface at a time: the shell's view goes down under the shell's own popover or the
    /// panel's app.
    pub(crate) fn clear_shell(&self) {
        self.caught_up();
        if self.overlays.here() == Overlay::Rest {
            return;
        }
        self.overlays.show(Overlay::Rest);
        self.settled();
    }

    /// Take the shell's own views down: the picker and the About sheet, and the surface and the
    /// popovers under them. One surface stands at a time, so opening another closes theirs.
    fn close_views(&self) {
        self.emoji.set(false);
        self.about.set(false);
        self.clear_shell();
        self.clear_panel();
    }

    /// Whether the shell's own view draws the emoji picker now.
    pub(crate) fn emoji_open(&self) -> bool {
        self.emoji.get()
    }

    /// Whether the shell's own view draws the About sheet now.
    pub(crate) fn about_open(&self) -> bool {
        self.about.get()
    }

    /// Whether any of the shell's own popovers stands over the windows now.
    pub(crate) fn beside_up(&self) -> bool {
        Part::ALL
            .into_iter()
            .filter(|part| !self.drawn_by_the_shell(*part))
            .any(|part| part.stands_beside() && self.overlays.is_open(part))
    }

    /// Whether the shell's own view draws `part` instead of the shell, so what the shell keeps in
    /// that part stands over nothing: the shell's launcher is the search under the shell's screen,
    /// as the quick settings are the shell's view of them. A popover nobody draws is not a surface
    /// the person is looking at, so it neither takes the shell's surface down nor keeps its keys.
    fn drawn_by_the_shell(&self, part: Part) -> bool {
        self.shell && part == Part::Search
    }

    /// Close `surface` (`close-surface`), when it is the one up.
    pub(crate) fn close(&self, surface: &str) {
        self.caught_up();
        if surface == EMOJI {
            self.emoji.set(false);
            return;
        }
        if surface == ABOUT {
            self.about.set(false);
            return;
        }
        if self.shell_centre(surface) {
            self.notices.ask_centre(false);
            return;
        }
        if self.panel_shows(surface) {
            self.overlays.close(Part::QuickSettings);
            return;
        }
        if let Some(surface) = Overlay::named(surface) {
            self.overlays.hide(surface);
        }
        self.settled();
    }

    /// Close the panel when a surface of the shell's own opens: one surface stands at a time,
    /// so the centre is not opened behind the panel that asked for it.
    fn panel_closes(&self) {
        if self.overlays.is_open(Part::QuickSettings) {
            self.overlays.close(Part::QuickSettings);
        }
    }

    /// Read the overview as the flag has it before a surface turns: the shell's own chrome opens and
    /// closes it too (its action, its keys, Escape), and a press answers what the person sees.
    fn caught_up(&self) {
        let here = self.overlays.here() == Overlay::Overview;
        if self.overview.get() && !here {
            self.overlays.show(Overlay::Overview);
        } else if !self.overview.get() && here {
            self.overlays.show(Overlay::Rest);
        }
    }

    /// Keep the shell's reading of the overview in step with the surface just changed: the shell
    /// folds behind it and its state names it as up the moment the person's press opens it,
    /// instead of after the shell's next frame.
    fn settled(&self) {
        self.overview.set(self.overlays.here() == Overlay::Overview);
    }
}

/// The styles every installed package offers, with the revision that moves when the
/// session re-reads its store: a package installed or taken away moves the shell's reading.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Styles {
    pub(crate) revision: u64,
    pub(crate) list: Vec<crate::styles::Style>,
    /// The store's registry the list was read at: a store that reads the same is not read again,
    /// and what the log says of it is said only when it changes.
    pub(crate) registry: Option<crate::styles::Registry>,
}

/// The shell as the session reads it. See the module docs.
pub(crate) struct DeviceLists {
    pub(crate) handle: DesktopHandle,
    pub(crate) person: String,
    /// The surfaces the shell shows over its windows, and where they stand.
    pub(crate) surfaces: Surfaces,
    /// The apps' live activities: Clocks' timer is the bar's clock beside the time, and the person
    /// putting the floating one away hides it until Clocks shows something new.
    pub(crate) activities: Rc<crate::chrome::Activities>,
    /// The running applications' tray items, when their watcher serves: the bar shows them,
    /// and the person's clicks go back through it.
    pub(crate) tray: Option<std::sync::Arc<tilo_tray::Tray>>,
    /// The agents the person admitted at its MCP server: each works here as itself, and the bar
    /// shows it as its own presence.
    pub(crate) agent_sessions: std::sync::Arc<crate::agent_sessions::AgentSessions>,
    /// This device's workspaces' record, when the session holds one: the workspace whose holder
    /// decides an invitation is asked there, and the links out of it keep this device dialable.
    pub(crate) engine: Option<std::sync::Arc<crate::engine::Engine>>,
    /// What the shell asks the session for while it runs: opening the invite sheet, which is the
    /// session's own surface and not a layer the shell draws.
    pub(crate) asks: Rc<std::cell::RefCell<Vec<tilo_desktop::desktop_host::DesktopAsk>>>,
    /// The styles every installed package offers, as `device.styles` reads them
    /// (shell.md §6): what the person's switch offers. The session fills it from its package
    /// store, and moves it when a package is installed or taken away.
    pub(crate) styles: std::rc::Rc<std::cell::RefCell<Styles>>,
    /// The workspaces' folders on this computer, as the files daemon last said each stands.
    pub(crate) folders: tilo_workspace_service::files::Folders,
    /// What the lists were last published from.
    pub(crate) lists: Option<ListsKey>,
}

/// The pieces a session's shell is made of, once: the same agent sessions, surfaces and records
/// reach the shell's reading and every part of the shell.
pub(crate) struct Shared {
    pub(crate) agent_sessions: std::sync::Arc<crate::agent_sessions::AgentSessions>,
    pub(crate) handle: DesktopHandle,
    pub(crate) person: String,
    pub(crate) activities: Rc<crate::chrome::Activities>,
    pub(crate) surfaces: Surfaces,
    pub(crate) tray: Option<std::sync::Arc<tilo_tray::Tray>>,
    pub(crate) engine: Option<std::sync::Arc<crate::engine::Engine>>,
    pub(crate) asks: Rc<std::cell::RefCell<Vec<tilo_desktop::desktop_host::DesktopAsk>>>,
    pub(crate) folders: tilo_workspace_service::files::Folders,
}

/// A factory making the session's shell, its styles read from the person's store as they move
/// (shell.md §6): called once for the shell's own service and once for the shell, each a view of
/// the same agent sessions, surfaces and records.
pub(crate) fn device_lists_from(shared: Shared) -> impl Fn() -> DeviceLists {
    let styles = Rc::new(std::cell::RefCell::new(Styles::default()));
    move || DeviceLists {
        agent_sessions: std::sync::Arc::clone(&shared.agent_sessions),
        handle: shared.handle.clone(),
        person: shared.person.clone(),
        activities: Rc::clone(&shared.activities),
        surfaces: shared.surfaces.clone(),
        tray: shared.tray.clone(),
        engine: shared.engine.clone(),
        asks: Rc::clone(&shared.asks),
        styles: Rc::clone(&styles),
        folders: shared.folders.clone(),
        lists: None,
    }
}

impl DeviceLists {
    /// What the lists are made from, without making them.
    fn lists_key(&self) -> ListsKey {
        ListsKey([
            (
                self.handle.revision(),
                // Which apps stopped answering is the compositor's: it changes without the tree.
                self.handle.unresponsive_revision(),
                self.surfaces.overlays.revision(),
                self.surfaces.overlays.revealed(tilo_clock::now()),
            ),
            (
                self.tray.as_ref().map_or(0, |tray| tray.revision()),
                0,
                0,
                false,
            ),
            (self.agent_sessions.revision(), 0, 0, false),
            (self.styles.borrow().revision, 0, 0, false),
            (self.folders.revision(), 0, 0, false),
        ])
    }

    /// The reading of one of the lists, by its resource.
    fn list(&self, resource: &str) -> Option<Value> {
        match resource {
            WINDOWS_RESOURCE => Some(reading(&crate::windows::Windows {
                all: self
                    .handle
                    .read(|tree| self.windows_reaching(tree, tilo_clock::now())),
            })),
            TRAY_RESOURCE => Some(reading(&Tray {
                items: self.tray_items(),
            })),
            AGENTS_RESOURCE => Some(reading(&crate::agent_sessions::Agents {
                sessions: self.agent_sessions.readings(),
            })),
            STYLES_RESOURCE => Some(reading(&OfferedStyles {
                offered: self.styles.borrow().list.clone(),
            })),
            FOLDERS_RESOURCE => Some(reading(&Folders {
                folders: self.folders.all().into_iter().map(Folder::from).collect(),
            })),
            _ => None,
        }
    }

    /// When the shell has something to draw that it has not drawn yet, without a frame in between:
    /// the instant the switcher's panel comes up while Alt is held on it.
    pub(crate) fn next_deadline(&self) -> Option<tilo_clock::time::Instant> {
        self.surfaces.overlays.deadline(tilo_clock::now())
    }

    /// The shell's windows, with the one the switcher's walk has reached marked focused while its
    /// panel shows: the tile its walk chose is the one lifted. A quick flick shows no panel, so it
    /// lifts nothing either.
    fn windows_reaching(
        &self,
        tree: &Layout,
        now: tilo_clock::time::Instant,
    ) -> Vec<crate::windows::Window> {
        let mut windows = windows(tree, &self.handle);
        // The app as people know it, never the id: a window is named by what it shows, and its
        // `app_title` is a person's own name for the window, not its app's name.
        for window in &mut windows {
            window.origin_app = crate::app_names::app_name(&window.app_key);
        }
        if self.surfaces.overlays.revealed(now)
            && let Some(at) = reaching(&self.surfaces.overlays, &windows, tree.active)
        {
            for (index, window) in windows.iter_mut().enumerate() {
                window.focused = index == at;
            }
        }
        windows
    }

    /// The due event Clocks wants announced, by its id and in the app's own words when it has them
    /// (the desktop's otherwise): what the bar announces once.
    pub(crate) fn due_timer(&self) -> Option<(String, String, String)> {
        let announced = self
            .activities
            .published(crate::chrome::CLOCKS_APP)?
            .announce?;
        let title = if announced.title.is_empty() {
            crate::i18n::t("clock-due-title")
        } else {
            announced.title
        };
        let body = if announced.body.is_empty() {
            crate::i18n::t("clock-due-body")
        } else {
            announced.body
        };
        Some((announced.id, title, body))
    }

    /// The running applications' tray items, in the order they registered: each's key, the app's
    /// own words for it, whether choosing it opens a menu rather than its window, and the app's
    /// own unread count. An item the protocol asks to hide (`Passive`) is left out, as the bar
    /// draws none of it.
    fn tray_items(&self) -> Vec<TrayItem> {
        self.tray.as_ref().map_or_else(Vec::new, |tray| {
            tray.snapshot()
                .into_iter()
                .filter(|item| item.status != tilo_tray::ItemStatus::Passive)
                .map(|item| {
                    let badge = item
                        .address
                        .owner_app
                        .as_deref()
                        .map_or(0, |app| i64::from(self.surfaces.notices.app_badge(app)));
                    TrayItem {
                        key: item.key,
                        title: if item.title.trim().is_empty() {
                            item.id
                        } else {
                            item.title
                        },
                        menu: item.item_is_menu,
                        badge,
                        attention: item.status == tilo_tray::ItemStatus::NeedsAttention,
                    }
                })
                .collect()
        })
    }
}

impl DeviceLists {
    /// The id of the shell's workspace `key`, or of the workspace in front when the caller names
    /// none.
    fn workspace_id(&self, workspace: Option<&str>) -> Result<WorkspaceId, ApiError> {
        let workspace_id = self.handle.read(|tree| {
            let workspace = match workspace {
                Some(key) => tree
                    .workspaces
                    .iter()
                    .find(|candidate| candidate.id().to_string() == key),
                None => tree.active_workspace(),
            };
            workspace.map(Workspace::id)
        });
        workspace_id.ok_or_else(|| match workspace {
            Some(key) => {
                ApiError::InvalidArguments(format!("there is no workspace {key} on this device"))
            }
            None => ApiError::Failed("no workspace is in front".to_owned()),
        })
    }

    /// The invitations into a workspace this device's person made that are still redeemable, as the
    /// workspace's host lists them: its id in hex, the part it grants, whom it is addressed to, how
    /// it names them and when it lapses. The same list says whether a link is out, which keeps
    /// this device dialable ([`crate::engine::Engine::invitations_open`]).
    fn invitations(&self, workspace: Option<&str>) -> Result<Started, ApiError> {
        let engine = self.engine()?;
        let workspace_id = self.workspace_id(workspace)?;
        let asked = engine
            .ask_holder(
                workspace_id,
                tilo_workspace_service::workspaces::INVITATIONS,
                serde_json::Map::new(),
            )
            .ok_or_else(no_holder)?;
        let noting = std::sync::Arc::clone(engine);
        Ok(answered_by_service(asked, move |value| {
            noting.note_invitations(workspace_id, &value);
            Ok(value)
        }))
    }

    /// Cancel one of them, from the device that issued it: the link admits nobody from now on.
    fn cancel_invitation(
        &self,
        invite: &str,
        workspace: Option<&str>,
    ) -> Result<Started, ApiError> {
        let engine = self.engine()?;
        let workspace_id = self.workspace_id(workspace)?;
        let mut arguments = serde_json::Map::new();
        arguments.insert("invite".to_owned(), invite.into());
        let asked = engine
            .ask_holder(
                workspace_id,
                tilo_workspace_service::workspaces::CANCEL_INVITATION,
                arguments,
            )
            .ok_or_else(no_holder)?;
        Ok(answered_by_service(asked, Ok))
    }

    /// Hand out a link into a workspace, made by this device's own person: the invitation the shell
    /// lists from now on, in the text another device joins with.
    fn issue_invitation(
        &self,
        role: Option<&str>,
        invitee_name: Option<String>,
        recipient: Option<&str>,
        expires_in_seconds: Option<u64>,
        workspace: Option<&str>,
    ) -> Result<Started, ApiError> {
        let engine = self.engine()?;
        let workspace_id = self.workspace_id(workspace)?;
        // A workspace whose governance this device reads, and which does not let this person
        // invite, is refused for that reason, before it reaches anywhere: what an app shares is
        // what its person may hand out, and a member who may not invite learns which part of the
        // ask they lack. A workspace still being taken in has no part to read here yet; the ask
        // waits for its holder, which judges the invitation once the part is held.
        if engine.invite_refused(&workspace_id.to_string()) {
            return Err(ApiError::Failed(
                "the person's part in this workspace may not invite".to_owned(),
            ));
        }
        let (role, makes_administrator) = match role {
            None => (Role::Editor, false),
            // An administrator joins as an Editor and is made one by the
            // invitation's own grant, which only someone who may make
            // administrators can issue.
            Some("administrator") => (Role::Editor, true),
            Some(name) => (
                Role::parse(name)
                    .filter(|role| role.assignable())
                    .ok_or_else(|| {
                        ApiError::InvalidArguments(format!("`{name}` is no part of a workspace"))
                    })?,
                false,
            ),
        };
        let recipient = recipient
            .map(|person| {
                let not_a_key = || {
                    ApiError::InvalidArguments(format!(
                        "`{person}` is not a person's key, 64 hex characters"
                    ))
                };
                let key = hex::decode(person).map_err(|_| not_a_key())?;
                <[u8; 32]>::try_from(key.as_slice()).map_err(|_| not_a_key())
            })
            .transpose()?;
        let invitee_name = match (invitee_name, recipient) {
            (None, Some(recipient)) => engine.name_of(&workspace_id.to_string(), recipient),
            (name, _) => name,
        };
        let expires_in = expires_in_seconds.unwrap_or(DEFAULT_INVITE_SECONDS);
        let mut arguments = serde_json::Map::new();
        let place = if makes_administrator {
            "administrator"
        } else {
            role.key()
        };
        arguments.insert("role".to_owned(), place.into());
        arguments.insert("expires_in_seconds".to_owned(), expires_in.into());
        if let Some(name) = &invitee_name {
            arguments.insert("invitee_name".to_owned(), name.clone().into());
        }
        if let Some(recipient) = recipient {
            arguments.insert("recipient".to_owned(), hex::encode(recipient).into());
        }
        let asked = engine
            .ask_holder(
                workspace_id,
                tilo_action_policy::roles::governance::INVITE,
                arguments,
            )
            .ok_or_else(no_holder)?;
        let noting = std::sync::Arc::clone(engine);
        let role = role.key();
        Ok(answered_by_service(asked, move |issued| {
            let signed = issued
                .get("invitation")
                .and_then(Value::as_str)
                .map(hex::decode)
                .transpose()
                .map_err(|_| "the workspace's holder sent an invalid invitation".to_owned())?
                .ok_or("the workspace's holder answered no invitation")?;
            // Older holders do not serve the compact lookup, so use the signed link they can admit.
            let link = issued
                .get("native_link")
                .and_then(Value::as_str)
                .map_or_else(
                    || tilo_peer_state::join::native_invitation_link(&signed),
                    str::to_owned,
                );
            // Whoever redeems it dials this device.
            if let Some(lapses) = issued.get("expires_at_ns").and_then(Value::as_u64) {
                noting.note_link(workspace_id, lapses);
            }
            Ok(serde_json::json!({
                "invite": issued.get("invite"),
                "link": link,
                "invitation": tilo_peer_state::join::encode_invitation_text(&signed),
                "role": role,
                "administrator": makes_administrator,
                "expires_at_ns": issued.get("expires_at_ns"),
            }))
        }))
    }

    /// What an invitation verb answers, when `effect` is one of them: the links that still work,
    /// one made, or one taken back.
    fn invitation(&self, effect: &InviteEffect) -> Option<Result<Started, ApiError>> {
        match effect {
            InviteEffect::Invitations { workspace } => Some(self.invitations(workspace.as_deref())),
            InviteEffect::IssueInvitation {
                role,
                invitee_name,
                recipient,
                expires_in_seconds,
                workspace,
            } => Some(self.issue_invitation(
                role.as_deref(),
                invitee_name.clone(),
                recipient.as_deref(),
                *expires_in_seconds,
                workspace.as_deref(),
            )),
            InviteEffect::CancelInvitation { invite, workspace } => {
                Some(self.cancel_invitation(invite, workspace.as_deref()))
            }
            _ => None,
        }
    }

    /// This device's engine: the workspace's holder is asked through it, and a session without one
    /// reaches no holder.
    fn engine(&self) -> Result<&std::sync::Arc<crate::engine::Engine>, ApiError> {
        self.engine
            .as_ref()
            .ok_or_else(|| ApiError::Failed("this session holds no workspace's record".to_owned()))
    }
}

/// What an invitation verb answers when this device reaches none of the workspace's holders: its
/// invitations are made, listed and taken back only there.
fn no_holder() -> ApiError {
    ApiError::Failed("this device reaches no holder of the workspace".to_owned())
}

impl DeviceLists {
    /// Whether this computer accepts the person's agents from their other devices, and whether
    /// their steps go unasked here, as the grant it keeps in its workspaces says.
    fn other_device_agents(&self) -> Started {
        let Some(engine) = &self.engine else {
            return other_device_agents_reading(false, false);
        };
        let device = engine.identity().device_cert.device_public_key;
        let answers: Vec<Option<Answer>> = self
            .handle
            .read(|tree| {
                tree.workspaces
                    .iter()
                    .map(|workspace| workspace.id().to_string())
                    .collect::<Vec<_>>()
            })
            .into_iter()
            .filter_map(|workspace| engine.policy(&workspace).ok())
            .map(|policy| tilo_action_policy::device_agents::answer(&policy, &device))
            .collect();
        let accept = !answers.is_empty() && answers.iter().all(Option::is_some);
        let unasked = accept && answers.iter().all(|answer| *answer == Some(Answer::Allow));
        other_device_agents_reading(accept, unasked)
    }

    /// Keep, change or take back the grant this computer's workspaces keep for the person's
    /// agents from their other devices ([`tilo_action_policy::device_agents`]): `Some(Ask)`
    /// accepts them with a step their grants ask first put to the person here, `Some(Allow)` lets
    /// those steps go unasked, and `None` refuses them.
    fn set_other_device_agents(&self, answer: Option<Answer>) -> Result<Started, ApiError> {
        let Some(engine) = self.engine.clone() else {
            return Err(ApiError::Failed(
                "this session has no workspace to answer in".to_owned(),
            ));
        };
        let device = engine.identity().device_cert.device_public_key;
        let person = self.person.clone();
        let workspaces: Vec<String> = self.handle.read(|tree| {
            tree.workspaces
                .iter()
                .map(|workspace| workspace.id().to_string())
                .collect::<Vec<_>>()
        });
        Ok(Started::Later(crate::landing::off_loop(async move {
            for workspace in workspaces {
                let Some(id) = WorkspaceId::parse(&workspace) else {
                    continue;
                };
                let current = engine
                    .policy(&workspace)
                    .ok()
                    .and_then(|policy| tilo_action_policy::device_agents::answer(&policy, &device));
                if current == answer {
                    continue;
                }
                let mut changes = Vec::new();
                if current.is_some() {
                    changes.push(Command::Revoke {
                        grant: tilo_action_policy::device_agents::grant_id(&device),
                    });
                }
                if let Some(answer) = answer {
                    changes.push(Command::Grant {
                        grant: tilo_action_policy::device_agents::grant(
                            &workspace,
                            &device,
                            answer,
                            Grantee::person(person.clone()),
                        ),
                    });
                }
                if changes.is_empty() {
                    continue;
                }
                tilo_session_client::membership::Membership::of(&engine, id)
                    .write(&changes)
                    .await
                    .map_err(ApiError::Failed)?;
            }
            Ok(tilo_actions::ActionOutput::default())
        })))
    }
}

/// What this computer answers for the person's agents from their other devices.
fn other_device_agents_reading(accept: bool, unasked: bool) -> Started {
    Started::Done(tilo_actions::ActionOutput {
        value: Some(serde_json::json!({ "accept": accept, "unasked": unasked })),
        resources: Vec::new(),
    })
}

impl DeviceLists {
    /// Open the invite sheet on a workspace: the workspace comes to the front, and the sheet the
    /// bar's button opens is shown on it, through the session, which holds the surface.
    fn show_invite(&self, workspace: Option<&str>) -> Result<Started, ApiError> {
        let workspace = match workspace {
            Some(key) => key.to_owned(),
            None => self
                .handle
                .read(|tree| {
                    tree.active_workspace()
                        .map(|active| active.id().to_string())
                })
                .ok_or_else(|| ApiError::Failed("no workspace is in front".to_owned()))?,
        };
        let known = self.handle.read(|tree| {
            tree.workspaces
                .iter()
                .any(|candidate| candidate.id().to_string() == workspace)
        });
        if !known {
            return Err(ApiError::InvalidArguments(format!(
                "there is no workspace {workspace} on this device"
            )));
        }
        let active = self.handle.read(|tree| {
            tree.active_workspace()
                .map(|active| active.id().to_string())
        });
        if active.as_deref() != Some(workspace.as_str()) {
            self.handle
                .dispatch(ChromeEffect::SwitchWorkspace {
                    workspace: Some(workspace),
                    delta: None,
                })
                .map_err(ApiError::Failed)?;
        }
        self.asks
            .borrow_mut()
            .push(DesktopAsk::Intent(ChromeIntent::Invite));
        Ok(Started::Done(tilo_actions::ActionOutput::default()))
    }
}

/// A pending invitation as listing invitations reports it, in the workspace host's own words: a
/// client that lists them from a host reads the same fields.
/// What this device's own workspace service answered `asked`, as `shape` puts it.
fn answered_by_service(
    asked: tilo_session_client::engine::ServiceAnswer,
    shape: impl FnOnce(Value) -> Result<Value, String> + Send + 'static,
) -> Started {
    Started::Later(crate::landing::off_loop(async move {
        let value = asked.await.and_then(shape).map_err(ApiError::Failed)?;
        Ok(tilo_actions::ActionOutput {
            value: Some(value),
            resources: Vec::new(),
        })
    }))
}

/// How long an invitation the shell hands out without being told otherwise works for: an hour, as a
/// person's invitation does.
const DEFAULT_INVITE_SECONDS: u64 = 3_600;

impl DeviceLists {
    /// Open or close the running application's tray item `key`, as a press on its icon does: its
    /// window, or its menu when the item is one.
    fn open_tray_item(&self, key: &str) -> Result<Started, ApiError> {
        let Some(tray) = &self.tray else {
            return Err(ApiError::Failed(
                "no tray is served: the press reaches no application".to_owned(),
            ));
        };
        let Some(item) = tray.snapshot().into_iter().find(|item| item.key == key) else {
            return Err(ApiError::InvalidArguments(format!(
                "there is no tray item {key}"
            )));
        };
        let request = if item.item_is_menu {
            tilo_tray::TrayRequest::ContextMenu { x: 0, y: 0 }
        } else {
            tilo_tray::TrayRequest::Activate { x: 0, y: 0 }
        };
        tray.request_or_else(key, request, |_| {})
            .map_err(ApiError::Failed)?;
        Ok(Started::Done(tilo_actions::ActionOutput::default()))
    }
}

impl DeviceLists {
    /// Carry out `effect` of the inviting resource.
    fn invite(&self, effect: &InviteEffect) -> Result<Started, ApiError> {
        if let Some(answer) = self.invitation(effect) {
            return answer;
        }
        match effect {
            InviteEffect::ShowInvite { workspace } => {
                return self.show_invite(workspace.as_deref());
            }
            InviteEffect::ShowPairDevice => self
                .asks
                .borrow_mut()
                .push(DesktopAsk::Intent(ChromeIntent::PairDevice)),
            InviteEffect::ShowConnect => self
                .handle
                .choose_host_action(tilo_actions::remote::session_actions::CONNECT_AGENT),
            _ => {}
        }
        Ok(Started::Done(tilo_actions::ActionOutput::default()))
    }
}

/// The session's own lists as it publishes them: what each is to search and read, under the
/// session's kind.
#[derive(tilo_resource::Row)]
#[row(kind = "session", label = "Windows")]
pub(crate) struct WindowsList;

#[derive(tilo_resource::Row)]
#[row(kind = "session", label = "Tray", effects = TrayEffect)]
pub(crate) struct TrayRoot;

#[derive(tilo_resource::Row)]
#[row(kind = "session", label = "Agents", effects = AgentsEffect)]
pub(crate) struct AgentsRoot;

#[derive(tilo_resource::Row)]
#[row(kind = "session", label = "Invite", effects = InviteEffect)]
pub(crate) struct InviteRoot;

#[derive(tilo_resource::Row)]
#[row(kind = "session", label = "Styles")]
pub(crate) struct StylesRoot;

#[derive(tilo_resource::Row)]
#[row(kind = "session", label = "Folders")]
pub(crate) struct FoldersRoot;

impl Service for DeviceLists {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![
            tilo_resource::LiveRow::typed::<WindowsList>(
                WINDOWS_RESOURCE,
                crate::i18n::t("session-windows"),
            )
            .into_resource(),
            offer::<TrayEffect>(
                tilo_resource::LiveRow::typed::<TrayRoot>(
                    TRAY_RESOURCE,
                    crate::i18n::t("session-tray"),
                )
                .into_resource(),
            ),
            offer::<AgentsEffect>(
                tilo_resource::LiveRow::typed::<AgentsRoot>(
                    AGENTS_RESOURCE,
                    crate::i18n::t("session-agents"),
                )
                .into_resource(),
            ),
            offer::<InviteEffect>(
                tilo_resource::LiveRow::typed::<InviteRoot>(
                    INVITE_RESOURCE,
                    crate::i18n::t("session-invite"),
                )
                .into_resource(),
            ),
            tilo_resource::LiveRow::typed::<StylesRoot>(
                STYLES_RESOURCE,
                crate::i18n::t("session-styles"),
            )
            .into_resource(),
            tilo_resource::LiveRow::typed::<FoldersRoot>(
                FOLDERS_RESOURCE,
                crate::i18n::t("session-folders"),
            )
            .into_resource(),
        ]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        self.list(resource)
    }

    fn poll(&mut self) -> bool {
        let now = self.lists_key();
        if self.lists.as_ref() == Some(&now) {
            return false;
        }
        self.lists = Some(now);
        true
    }

    /// The instant the switcher's panel comes up while Alt is held: the windows' reading marks
    /// the one the walk reached then.
    fn wakes_in(&self) -> Option<std::time::Duration> {
        self.next_deadline()
            .map(|at| at.saturating_duration_since(tilo_clock::now()))
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        match resource {
            TRAY_RESOURCE => {
                let TrayEffect::OpenTrayItem { key } =
                    decode::<TrayEffect>(&self.resources(), resource, action, arguments)?;
                self.open_tray_item(&key)
            }
            AGENTS_RESOURCE => {
                let effect =
                    decode::<AgentsEffect>(&self.resources(), resource, action, arguments)?;
                match effect {
                    AgentsEffect::StopAgent { agent } => {
                        if !self
                            .agent_sessions
                            .stop(&tilo_action_policy::AgentSessionId::new(agent.clone()))
                        {
                            return Err(ApiError::InvalidArguments(format!(
                                "no agent session {agent} works here"
                            )));
                        }
                        Ok(Started::Done(tilo_actions::ActionOutput::default()))
                    }
                    AgentsEffect::OtherDeviceAgents => Ok(self.other_device_agents()),
                    AgentsEffect::SetAcceptOtherDeviceAgents { on } => {
                        self.set_other_device_agents(if on { Some(Answer::Ask) } else { None })
                    }
                    AgentsEffect::SetOtherDeviceAgentsUnasked { on } => self
                        .set_other_device_agents(Some(if on {
                            Answer::Allow
                        } else {
                            Answer::Ask
                        })),
                }
            }
            INVITE_RESOURCE => {
                let effect =
                    decode::<InviteEffect>(&self.resources(), resource, action, arguments)?;
                self.invite(&effect)
            }
            _ => Err(ApiError::Unknown(ActionAddress::new(resource, action))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window whose app stopped answering says so in the reading every view of the shell reads,
    /// and only for that window.
    #[test]
    fn a_window_whose_app_stopped_answering_says_so_in_the_reading() {
        struct Blank;

        impl tilo_desktop_surface::TileSource for Blank {
            fn tile(
                &mut self,
                _: &tilo_desktop_model::Workspace,
                tab: &tilo_desktop_model::Tab,
            ) -> ui::widget::TileHost {
                ui::widget::TileHost::new(ui::widget::TileContent::Notice {
                    title: tab.title.clone(),
                    detail: String::new(),
                })
            }
        }

        let mut minter = tilo_desktop_model::IdMinter::new(5);
        let tab = tilo_desktop_model::Tab {
            id: minter.mint(),
            title: "Notes".to_owned(),
            tile: tilo_desktop_model::TileContent::Foreign {
                app_id: "so.tilo.notes".to_owned(),
                surface_id: 77,
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        };
        let tree = tilo_session_core::desktop::seed_tree(&mut minter, "Shell", vec![tab]);
        let (_surface, handle) =
            tilo_desktop_surface::DesktopSurface::new(tree.clone(), 5, Box::new(Blank));
        assert!(!windows(&tree, &handle)[0].not_responding);
        assert!(handle.set_unresponsive_windows(vec![77]));
        assert!(windows(&tree, &handle)[0].not_responding);
        // Another window's silence is not this one's.
        assert!(handle.set_unresponsive_windows(vec![78]));
        assert!(!windows(&tree, &handle)[0].not_responding);
    }

    fn surfaces(status_panel: bool, shell: bool) -> Surfaces {
        struct Blank;

        impl tilo_desktop_surface::TileSource for Blank {
            fn tile(
                &mut self,
                _: &tilo_desktop_model::Workspace,
                tab: &tilo_desktop_model::Tab,
            ) -> ui::widget::TileHost {
                ui::widget::TileHost::new(ui::widget::TileContent::Notice {
                    title: tab.title.clone(),
                    detail: String::new(),
                })
            }
        }

        let (_surface, handle) =
            tilo_desktop_surface::DesktopSurface::new(Layout::default(), 1, Box::new(Blank));
        Surfaces {
            status_panel,
            shell,
            overlays: Overlays::new(),
            notices: tilo_desktop_surface::DesktopNotifications::default(),
            overview: Rc::new(std::cell::Cell::new(false)),
            handle,
            emoji: Rc::new(std::cell::Cell::new(false)),
            about: Rc::new(std::cell::Cell::new(false)),
        }
    }

    #[test]
    fn the_overview_turns_the_shells_reading_with_the_surface() {
        let surfaces = surfaces(false, true);
        assert!(!surfaces.overview.get());
        surfaces.toggle(Overlay::Overview.name());
        assert!(surfaces.overview.get(), "the overview is up");
        surfaces.toggle(Overlay::Overview.name());
        assert!(!surfaces.overview.get(), "a second press closes it");
        surfaces.open(Overlay::Overview.name());
        assert!(surfaces.overview.get());
        surfaces.open(Overlay::Centre.name());
        assert!(!surfaces.overview.get(), "another surface replaces it");
        surfaces.open(Overlay::Overview.name());
        surfaces.close(Overlay::Centre.name());
        assert!(surfaces.overview.get(), "closing another surface leaves it");
        surfaces.close(Overlay::Overview.name());
        assert!(!surfaces.overview.get());
    }

    /// The shell's chrome opens and closes the overview itself as well: a press then reads the
    /// overview as it is, and closing another surface leaves it up.
    #[test]
    fn the_surfaces_read_the_overview_the_chrome_opened() {
        let surfaces = surfaces(false, false);
        surfaces.overview.set(true);
        surfaces.close(Overlay::Launcher.name());
        assert!(surfaces.overview.get(), "closing another surface leaves it");
        surfaces.toggle(Overlay::Overview.name());
        assert!(!surfaces.overview.get(), "a press closes the one up");
        surfaces.toggle(Overlay::Overview.name());
        surfaces.overview.set(false);
        surfaces.toggle(Overlay::Overview.name());
        assert!(
            surfaces.overview.get(),
            "a press opens the one Escape closed"
        );
    }

    #[test]
    fn a_surface_of_the_shells_own_closes_the_panel_that_asked_for_it() {
        let surfaces = surfaces(true, true);
        surfaces.open(crate::quick_settings::SURFACE);
        surfaces.open(Overlay::Centre.name());
        assert_eq!(surfaces.overlays.here(), Overlay::Centre);
    }

    /// The shell draws the one centre the bar's bell counts: a press on it asks the shell's own
    /// chrome, as the bell asks it, and puts the panel that asked away.
    #[test]
    fn the_shells_own_centre_is_opened_where_the_shell_draws_it() {
        let surfaces = surfaces(true, false);
        surfaces.open(crate::quick_settings::SURFACE);
        assert!(surfaces.overlays.is_open(Part::QuickSettings));
        surfaces.toggle(Overlay::Centre.name());
        assert_eq!(
            surfaces.notices.take_centre_ask(),
            Some(true),
            "the shell's chrome is asked to open the centre"
        );
        assert_eq!(
            surfaces.overlays.here(),
            Overlay::Rest,
            "the shell draws no centre of its own over a shell that draws one"
        );
        assert!(
            !surfaces.overlays.is_open(Part::QuickSettings),
            "one surface at a time: the centre is not opened behind the panel"
        );

        // The centre is up: a second press closes it, as the bell's next press does.
        surfaces.notices.set_centre_open(true);
        surfaces.toggle(Overlay::Centre.name());
        assert_eq!(surfaces.notices.take_centre_ask(), Some(false));

        // The shell's own screen keeps its own view of the centre.
        let screen = self::surfaces(true, true);
        screen.toggle(Overlay::Centre.name());
        assert_eq!(screen.overlays.here(), Overlay::Centre);
        assert_eq!(screen.notices.take_centre_ask(), None);
    }

    #[test]
    fn a_shell_draws_the_quick_settings_it_shows() {
        for status_panel in [false, true] {
            let surfaces = surfaces(status_panel, true);
            surfaces.toggle(crate::quick_settings::SURFACE);
            assert_eq!(surfaces.overlays.here(), Overlay::QuickSettings);
            assert!(!surfaces.overlays.is_open(Part::QuickSettings));
            surfaces.close(crate::quick_settings::SURFACE);
            assert_eq!(surfaces.overlays.here(), Overlay::Rest);
        }
    }

    #[test]
    fn the_shell_leaves_the_quick_settings_to_the_panels_app_it_has() {
        let surfaces = surfaces(true, false);
        surfaces.toggle(crate::quick_settings::SURFACE);
        assert!(surfaces.overlays.is_open(Part::QuickSettings));
        assert_eq!(surfaces.overlays.here(), Overlay::Rest);
        surfaces.close(crate::quick_settings::SURFACE);
        assert!(!surfaces.overlays.is_open(Part::QuickSettings));
    }

    #[test]
    fn without_the_panels_app_the_shell_shows_the_shells_quick_settings() {
        let surfaces = surfaces(false, false);
        surfaces.open(crate::quick_settings::SURFACE);
        assert_eq!(surfaces.overlays.here(), Overlay::QuickSettings);
        assert!(!surfaces.overlays.is_open(Part::QuickSettings));
    }
}
