//! This device's lists as resources of the session's: its windows, the tray, the agents at the
//! person's peer network and the styles, and inviting into a workspace. A shell's views read them, and
//! so does a script or an agent, through the API, by the same addresses: one reading for every
//! reader.

use std::rc::Rc;

use serde_json::{Map, Value};
use tilo_actions::remote::session_actions::INVITE_RESOURCE;
use tilo_actions::{ActionAddress, ApiError, ResourceSpec};
use tilo_desktop_model::{Layout, Workspace, WorkspaceId};
use tilo_peer_state::join::Role;
use tilo_resource::Effect;
use tilo_service::{Service, Started, decode, offer, reading};
use tilo_session_core::desktop::host::chrome::ChromeIntent;
use tilo_session_core::desktop::host::contract::DesktopAsk;
use tilo_session_core::desktop::navigation::ChromeEffect;
use tilo_session_core::desktop::overlays::{Overlay, Overlays, Part, SwitcherScope};
use tilo_session_core::desktop::state::DesktopHandle;

use crate::followed_resource::FollowedReading as Followed;
use crate::windows::{reaching, windows};

/// What the device's lists are made from ([`DeviceLists::lists_key`]), one entry per resource: the
/// windows, the tray, the agents and the styles. An entry that moved is a reading that moved.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ListsKey([(u64, u64, u64, bool); 8]);

/// The shell's windows, every workspace's: what the switcher, the dock and the launcher list.
pub(crate) const WINDOWS_RESOURCE: &str = "session:windows";
/// The running applications' tray items.
pub(crate) const TRAY_RESOURCE: &str = "session:tray";
/// The agents the person admitted at their MCP server.
pub(crate) const AGENTS_RESOURCE: &str = "session:agents";
/// The styles the device's installed packages offer, as the device's daemon publishes them in the
/// person's own workspace, with what failed in this session's shell marked on them.
pub(crate) const STYLES_RESOURCE: &str = tilo_workspace_service::styles::STYLES_APP;
/// The workspaces' folders on this computer, and whether what is put in them reached the server.
pub(crate) const FOLDERS_RESOURCE: &str = "session:folders";
/// The shell's open search: what is typed and the rows it found, as the launcher shows them.
pub(crate) const SEARCH_RESOURCE: &str = "session:search";
/// The empty shell's welcome cards, as the launcher offers them.
pub(crate) const LAUNCHPAD_RESOURCE: &str = "session:launchpad";
/// The notice the shell says in passing over the windows: what just happened, and what the person
/// can still do about it.
pub(crate) const NOTICE_RESOURCE: &str = "session:notice";
/// The picture the workspace in front shows behind its windows, as a view reads it.
pub(crate) const WALLPAPER_RESOURCE: &str = "session:wallpaper";

pub(crate) use tilo_device_surfaces::lists::{
    AgentsEffect, Folder, Folders, Launchpad, LaunchpadCard, SWITCHER_SCOPES, Said, SaidAction,
    SaidEffect, Search, SearchGroup, SearchResult, Tray, TrayEffect, TrayItem, Wallpaper,
};

/// The glyph a welcome card shows: the app's own glyph where it opens one, and the shell's own for
/// inviting someone and catching up.
fn card_icon(card: tilo_session_core::desktop::launchpad::LaunchpadAction) -> String {
    match card.app_target() {
        Some((app, _)) => ui::icons::app_glyph(app, None).name().to_owned(),
        None if card.slug() == "invite" => ui::icons::Icon::UserPlus.name().to_owned(),
        None => ui::icons::Icon::History.name().to_owned(),
    }
}

/// The glyph a search kind's rows show: its kind's own, as the palette draws it.
fn kind_icon(kind: &str) -> String {
    use ui::search::system::{ResourceKind, icon_for_kind};
    let kind = match kind {
        "Actions" => ResourceKind::Action,
        "Apps" => ResourceKind::Application,
        "Notes" => ResourceKind::Note,
        "Records" => ResourceKind::Record,
        "Files" => ResourceKind::File,
        "Folders" => ResourceKind::Folder,
        "Creators" => ResourceKind::Creator,
        "Videos" => ResourceKind::Video,
        "Playlists" => ResourceKind::Playlist,
        "Tabs" => ResourceKind::Tab,
        "Window groups" => ResourceKind::WindowGroup,
        other => ResourceKind::Custom(other.to_owned()),
    };
    icon_for_kind(&kind).name().to_owned()
}

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
    /// Show how a new device of the person's joins them: Settings' devices page, where "Add a
    /// device" prints the ticket it logs in with. An app that lists what acts as the person offers
    /// adding to it this way.
    #[effect(screen)]
    ShowPairDevice,
    /// Open what a `tilo://open#…` link names: the app that keeps it shows it, started first when
    /// no window of it does. An app that shows a link opens it this way, whoever it was copied
    /// from.
    #[effect(screen)]
    OpenLink {
        /// The link, a `tilo://open#…` address
        #[param("The link, a `tilo://open#…` address")]
        link: String,
    },
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

/// The surfaces the shell shows over its windows: the shell's own views.
/// What `show-surface` and `close-surface` open and close, whether the person asks on the
/// shell's surface or on the chrome's above it, so both open the one surface.
#[derive(Clone)]
pub(crate) struct Surfaces {
    /// The stack the panel's own app is placed in, with the shell's surface up.
    pub(crate) overlays: Overlays,
    /// The notifications the shell's own chrome counts and draws: where the one centre opens
    /// when the shell draws it, as the bar's bell opens it.
    pub(crate) notices: tilo_session_core::desktop::state::DesktopNotifications,
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
    /// Whether the power menu stands over the windows, as the shell's own view draws it: a popover
    /// under the bar's trailing end, which the session puts up from a chord, the panel's power
    /// button and its own published verb.
    pub(crate) power: Rc<std::cell::Cell<bool>>,
}

/// The name the shell's own view gives the emoji picker, in what stands over the windows.
pub(crate) const EMOJI: &str = "emoji";
/// The name the shell's own view gives the About sheet, in what stands over the windows.
pub(crate) const ABOUT: &str = "about";
/// The name the shell's own view gives the power menu, in what stands over the windows.
pub(crate) const POWER: &str = "power";
/// The surface a held Super's key card makes the shell say it shows: a picture over the windows,
/// which takes no key.
pub(crate) const PEEK: &str = tilo_session_core::shell::PEEK_SURFACE;

impl Surfaces {
    /// The strips list's name in the stack: `show-strips` opens it and choosing a strip closes it.
    pub(crate) const STRIPS: &str = "strips";

    /// Show `surface` (`show-surface`): the one up closes, and any other opens.
    pub(crate) fn toggle(&self, surface: &str) {
        self.caught_up();
        if surface == Self::STRIPS {
            if self.overlays.is_open(Part::Strips) {
                self.overlays.close(Part::Strips);
            } else {
                self.close_views();
                self.overlays.open(Part::Strips, None);
            }
            self.settled();
            return;
        }
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
        if surface == POWER {
            let opening = !self.power.get();
            self.close_views();
            self.set_power(opening);
            self.settled();
            return;
        }
        // Another surface opens: the shell's own views go down under it.
        self.emoji.set(false);
        self.about.set(false);
        self.set_power(false);
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
        if name == Self::STRIPS {
            self.close_views();
            self.overlays.open(Part::Strips, None);
            self.settled();
            return;
        }
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
        if name == POWER {
            self.close_views();
            self.set_power(true);
            self.settled();
            return;
        }
        // Another surface opens: the shell's own views go down under it.
        self.emoji.set(false);
        self.about.set(false);
        self.set_power(false);
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

    /// Show the history on what the person opened lately of `app` or of `kind` (both empty for
    /// everything), whether or not it is up.
    pub(crate) fn history(&self, app: &str, kind: &str) {
        self.caught_up();
        self.panel_closes();
        self.overlays.history(app, kind);
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
        self.set_power(false);
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

    /// Put away what the shell shows over its windows, topmost first: the emoji picker, the About
    /// sheet, the strips list, or whichever surface the stack names. False when nothing stands over
    /// the windows. Escape answers here, so a key and the person's own press close the same way.
    pub(crate) fn close_top(&self) -> bool {
        if self.emoji.get() {
            self.emoji.set(false);
            return true;
        }
        if self.about.get() {
            self.about.set(false);
            return true;
        }
        if self.power.get() {
            self.set_power(false);
            return true;
        }
        if self.overlays.is_open(Part::Strips) {
            self.overlays.close(Part::Strips);
            return true;
        }
        if self.overlays.here() != Overlay::Rest {
            self.overlays.show(Overlay::Rest);
            self.settled();
            return true;
        }
        false
    }

    /// Whether the shell's own view draws the power menu now.
    pub(crate) fn power_open(&self) -> bool {
        self.power.get()
    }

    /// Put the power menu up or take it down: the flag the shell's reading draws from and the
    /// part of the stack Escape, a press outside and the keyboard answer are one state.
    pub(crate) fn set_power(&self, open: bool) {
        if self.power.get() == open {
            return;
        }
        self.power.set(open);
        if open {
            self.overlays.open(Part::Power, None);
        } else {
            self.overlays.close(Part::Power);
        }
    }

    /// Follow the stack with the shell's own power menu: a surface it closed (Escape, a press
    /// outside it, another surface coming up) is not one the shell still draws.
    pub(crate) fn settle_power(&self) {
        if self.power.get() && !self.overlays.is_open(Part::Power) {
            self.power.set(false);
            // The stack took it down and asked the widget that draws it to follow: the shell has,
            // and says so, so the part stands closed and may be opened again by a later press.
            self.overlays.close(Part::Power);
        }
    }

    /// Whether any of the shell's own popovers stands over the windows now.
    pub(crate) fn beside_up(&self) -> bool {
        Part::ALL
            .into_iter()
            .filter(|part| !Self::drawn_by_the_shell(*part))
            .any(|part| part.stands_beside() && self.overlays.is_open(part))
    }

    /// Whether the shell's own view draws `part` instead of the shell, so what the shell keeps in
    /// that part stands over nothing: the shell's launcher is the search under the shell's screen,
    /// as the quick settings are the shell's view of them. A popover nobody draws is not a surface
    /// the person is looking at, so it neither takes the shell's surface down nor keeps its keys.
    fn drawn_by_the_shell(part: Part) -> bool {
        part == Part::Search
    }

    /// Close `surface` (`close-surface`), when it is the one up.
    pub(crate) fn close(&self, surface: &str) {
        self.caught_up();
        if surface == Self::STRIPS {
            self.overlays.close(Part::Strips);
            self.settled();
            return;
        }
        if surface == EMOJI {
            self.emoji.set(false);
            return;
        }
        if surface == ABOUT {
            self.about.set(false);
            return;
        }
        if surface == POWER {
            self.set_power(false);
            self.settled();
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

/// The shell as the session reads it. See the module docs.
pub(crate) struct DeviceLists {
    /// The pictures the workspaces show, as the person chose them.
    pub(crate) wallpapers: std::sync::Arc<crate::wallpapers::Wallpapers>,
    pub(crate) handle: DesktopHandle,
    pub(crate) person: String,
    /// The surfaces the shell shows over its windows, and where they stand.
    pub(crate) surfaces: Surfaces,
    /// The apps' live activities: Clocks' timer is the bar's clock beside the time, and the person
    /// putting the floating one away hides it until Clocks shows something new.
    pub(crate) activities: Rc<crate::shell::activities::Activities>,
    /// What is live now, as `device.live` reads it.
    pub(crate) live: Rc<crate::live_list::Sources>,
    /// The running applications' tray items, when their watcher serves: the bar shows them,
    /// and the person's clicks go back through it.
    pub(crate) tray: Option<std::sync::Arc<tilo_tray::Tray>>,
    /// The agents the person admitted at its MCP server: each works here as itself, and the bar
    /// shows it as its own presence.
    pub(crate) agent_sessions: std::sync::Arc<crate::agent_sessions::AgentSessions>,
    /// How the person's workspaces stand, as the device's daemon publishes them: an invitation is
    /// asked of the workspace through it, and the links out of it keep this device dialable.
    pub(crate) view: Option<std::sync::Arc<crate::workspace_view::WorkspaceView>>,
    /// What the shell asks the session for while it runs: opening the invite sheet, which is the
    /// session's own surface and not a layer the shell draws.
    pub(crate) asks:
        Rc<std::cell::RefCell<Vec<tilo_session_core::desktop::host::contract::DesktopAsk>>>,
    /// The styles the device's installed packages offer, as its daemon publishes them in the
    /// person's own workspace (shell.md §6): what the person's switch offers. It moves when a
    /// package is installed or taken away.
    pub(crate) styles: Followed<tilo_workspace_service::styles::StyleList>,
    /// The styles that failed in this session's shell, marked on what it shows of them.
    pub(crate) failed_styles: Rc<std::cell::RefCell<crate::styles::Failed>>,
    /// The workspaces' folders on this computer, as the device's daemon last said each stands.
    pub(crate) folders: Followed<tilo_workspace_service::files::FolderList>,
    /// The apps this device lists, whose generation says which welcome cards stand here.
    pub(crate) lists_source: crate::lists::Lists,
    /// What the lists were last published from.
    pub(crate) lists: Option<ListsKey>,
    /// The passing notice last published, so one appearing, changing or going is a reading that
    /// moved.
    pub(crate) notice_seen: Option<ui::window_toolbar::ToolbarNotice>,
}

/// The pieces a session's shell is made of, once: the same agent sessions, surfaces and records
/// reach the shell's reading and every part of the shell.
pub(crate) struct Shared {
    pub(crate) agent_sessions: std::sync::Arc<crate::agent_sessions::AgentSessions>,
    pub(crate) handle: DesktopHandle,
    pub(crate) person: String,
    pub(crate) activities: Rc<crate::shell::activities::Activities>,
    pub(crate) live: Rc<crate::live_list::Sources>,
    pub(crate) surfaces: Surfaces,
    pub(crate) tray: Option<std::sync::Arc<tilo_tray::Tray>>,
    pub(crate) view: Option<std::sync::Arc<crate::workspace_view::WorkspaceView>>,
    pub(crate) asks:
        Rc<std::cell::RefCell<Vec<tilo_session_core::desktop::host::contract::DesktopAsk>>>,
    pub(crate) folders: Followed<tilo_workspace_service::files::FolderList>,
    pub(crate) styles: Followed<tilo_workspace_service::styles::StyleList>,
    pub(crate) lists: crate::lists::Lists,
    /// The pictures the workspaces show, as the person chose them: what the shell's wallpaper is.
    pub(crate) wallpapers: std::sync::Arc<crate::wallpapers::Wallpapers>,
}

/// A factory making the session's shell: called once for the shell's own service and once for the
/// shell, each a view of the same agent sessions, surfaces, records and styles.
pub(crate) fn device_lists_from(shared: Shared) -> impl Fn() -> DeviceLists {
    let failed_styles = Rc::new(std::cell::RefCell::new(crate::styles::Failed::default()));
    move || DeviceLists {
        agent_sessions: std::sync::Arc::clone(&shared.agent_sessions),
        handle: shared.handle.clone(),
        person: shared.person.clone(),
        activities: Rc::clone(&shared.activities),
        live: Rc::clone(&shared.live),
        surfaces: shared.surfaces.clone(),
        tray: shared.tray.clone(),
        view: shared.view.clone(),
        asks: Rc::clone(&shared.asks),
        styles: shared.styles.clone(),
        failed_styles: Rc::clone(&failed_styles),
        folders: shared.folders.clone(),
        lists_source: shared.lists.clone(),
        wallpapers: std::sync::Arc::clone(&shared.wallpapers),
        lists: None,
        notice_seen: None,
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
            (
                self.styles.revision(),
                self.failed_styles.borrow().revision,
                0,
                false,
            ),
            (self.folders.revision(), 0, 0, false),
            (self.handle.search_revision(), 0, 0, false),
            (self.lists_source.snapshot().0, 0, 0, false),
            {
                let (rows, board, playing) = self.live.revision();
                (rows, board, playing, false)
            },
        ])
    }

    /// The reading of one of the lists, by its resource.
    fn list(&self, resource: &str) -> Option<Value> {
        match resource {
            WINDOWS_RESOURCE => Some(reading(&crate::windows::Windows {
                all: self
                    .handle
                    .read(|tree| self.windows_reaching(tree, tilo_clock::now())),
                displays: self.handle.read(crate::windows::displays),
            })),
            TRAY_RESOURCE => Some(reading(&Tray {
                items: self.tray_items(),
            })),
            AGENTS_RESOURCE => Some(reading(&crate::agent_sessions::Agents {
                sessions: self.agent_sessions.readings(),
            })),
            STYLES_RESOURCE => Some(reading(&tilo_workspace_service::styles::StyleList {
                offered: self
                    .failed_styles
                    .borrow()
                    .marked(self.styles.get().offered),
            })),
            FOLDERS_RESOURCE => Some(reading(&Folders {
                folders: self
                    .folders
                    .get()
                    .folders
                    .into_iter()
                    .map(Folder::from)
                    .collect(),
            })),
            SEARCH_RESOURCE => Some(reading(&self.search())),
            WALLPAPER_RESOURCE => Some(reading(&self.wallpaper())),
            LAUNCHPAD_RESOURCE => Some(reading(&Launchpad {
                cards: crate::host_actions::cards(&self.lists_source)
                    .into_iter()
                    .map(|card| LaunchpadCard {
                        slug: card.slug().to_owned(),
                        label: card.label(),
                        icon: card_icon(card),
                    })
                    .collect(),
            })),
            NOTICE_RESOURCE => Some(reading(&self.said())),
            crate::live_list::RESOURCE => Some(reading(&self.live.read())),
            _ => None,
        }
    }

    /// The shell's open search, as the launcher lists it: the rows the search found and what it
    /// says about them.
    fn search(&self) -> Search {
        let open = self.handle.search_open();
        let (query, rows) = self.handle.search_rows();
        // The rows come ranked, kind by kind; a kind the ranking split (two availabilities) is
        // listed once, its rows kept in the order they came.
        let mut groups: Vec<SearchGroup> = Vec::new();
        for (at, row) in rows.into_iter().enumerate() {
            let index = i64::try_from(at).unwrap_or(i64::MAX);
            let result = SearchResult {
                index,
                title: row.title,
                subtitle: row.subtitle.unwrap_or_default(),
                facts: row.facts,
            };
            match groups.iter_mut().find(|group| group.kind == row.kind) {
                Some(group) => group.rows.push(result),
                None => groups.push(SearchGroup {
                    icon: kind_icon(&row.kind),
                    kind: row.kind,
                    rows: vec![result],
                }),
            }
        }
        Search {
            query,
            open,
            searching: open && self.handle.search_searching(),
            expecting: open && self.handle.search_expecting(),
            groups,
        }
    }

    /// The picture the workspace in front shows behind its windows, by the name the person's
    /// choice gives it.
    fn wallpaper(&self) -> Wallpaper {
        let active = self.handle.read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.id().to_string())
        });
        let picture = active
            .as_deref()
            .and_then(|workspace| self.wallpapers.value(workspace))
            .unwrap_or_default();
        Wallpaper { picture }
    }

    /// The passing notice the shell says over the windows now, as the reading spells it: empty
    /// while nothing just happened.
    fn said(&self) -> Said {
        self.handle
            .notice()
            .borrow()
            .get()
            .map(|notice| Said {
                text: notice.text.clone(),
                kind: serde_json::to_value(notice.kind)
                    .ok()
                    .and_then(|kind| kind.as_str().map(str::to_owned))
                    .unwrap_or_default(),
                actions: notice
                    .actions
                    .iter()
                    .map(|action| SaidAction {
                        id: action.id.clone(),
                        name: action.name.clone(),
                        icon: action.icon.clone(),
                    })
                    .collect(),
            })
            .unwrap_or_default()
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
            .published(crate::shell::activities::CLOCKS_APP)?
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
    /// this device dialable ([`crate::workspace_view::WorkspaceView::invitations_open`]).
    fn invitations(&self, workspace: Option<&str>) -> Result<Started, ApiError> {
        let view = self.view()?;
        let workspace_id = self.workspace_id(workspace)?;
        let asked = view
            .ask_holder(
                workspace_id,
                tilo_workspace_service::workspaces::INVITATIONS,
                serde_json::Map::new(),
            )
            .ok_or_else(no_holder)?;
        Ok(answered_by_service(asked, Ok))
    }

    /// Cancel one of them, from the device that issued it: the link admits nobody from now on.
    fn cancel_invitation(
        &self,
        invite: &str,
        workspace: Option<&str>,
    ) -> Result<Started, ApiError> {
        let view = self.view()?;
        let workspace_id = self.workspace_id(workspace)?;
        let mut arguments = serde_json::Map::new();
        arguments.insert("invite".to_owned(), invite.into());
        let asked = view
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
        let view = self.view()?;
        let workspace_id = self.workspace_id(workspace)?;
        // A workspace whose governance this device reads, and which does not let this person
        // invite, is refused for that reason, before it reaches anywhere: what an app shares is
        // what its person may hand out, and a member who may not invite learns which part of the
        // ask they lack. A workspace still being taken in has no part to read here yet; the ask
        // waits for its holder, which judges the invitation once the part is held.
        if view.invite_refused(&workspace_id.to_string()) {
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
            (None, Some(recipient)) => view.name_of(&workspace_id.to_string(), recipient),
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
        let asked = view
            .ask_holder(
                workspace_id,
                tilo_actions::remote::WORKSPACE_INVITE,
                arguments,
            )
            .ok_or_else(no_holder)?;
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

    /// The person's workspaces as the device's daemon keeps them: a workspace is asked through
    /// it, and a session that reaches no daemon asks none.
    fn view(&self) -> Result<&std::sync::Arc<crate::workspace_view::WorkspaceView>, ApiError> {
        self.view.as_ref().ok_or_else(|| {
            ApiError::Failed("this session reaches no daemon keeping its workspaces".to_owned())
        })
    }
}

/// What an invitation verb answers when this device reaches none of the workspace's holders: its
/// invitations are made, listed and taken back only there.
fn no_holder() -> ApiError {
    ApiError::Failed("this device reaches no holder of the workspace".to_owned())
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
            // A link opens where the shell's own open lands: the app that keeps what it names
            // shows it, on the desktop this session runs.
            InviteEffect::OpenLink { link } => self
                .asks
                .borrow_mut()
                .push(DesktopAsk::OpenLink(link.clone())),
            _ => {}
        }
        Ok(Started::Done(tilo_actions::ActionOutput::default()))
    }
}

/// The inviting resource as the session publishes it: the links into a workspace, the invite
/// sheet, connecting an agent or a computer, adding a device, and opening a link. A shell reads it
/// as `so.tilo.session`'s `invite`, and a package that shows a link calls [`InviteEffect::OpenLink`]
/// on it.
#[must_use]
pub(crate) fn invite_resource() -> ResourceSpec {
    offer::<InviteEffect>(
        tilo_resource::LiveRow::typed::<InviteRoot>(
            INVITE_RESOURCE,
            crate::i18n::t("session-invite"),
        )
        .into_resource(),
    )
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

#[derive(tilo_resource::Row)]
#[row(kind = "session", label = "Search")]
pub(crate) struct SearchRoot;

#[derive(tilo_resource::Row)]
#[row(kind = "session", label = "Launchpad")]
pub(crate) struct LaunchpadRoot;

#[derive(tilo_resource::Row)]
#[row(kind = "session", label = "Wallpaper")]
pub(crate) struct WallpaperRoot;

#[derive(tilo_resource::Row)]
#[row(kind = "session", label = "Notice", effects = SaidEffect)]
pub(crate) struct SaidRoot;

#[derive(tilo_resource::Row)]
#[row(kind = "session", label = "Live", effects = crate::live_list::LiveEffect)]
pub(crate) struct LiveRoot;

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
            invite_resource(),
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
            tilo_resource::LiveRow::typed::<SearchRoot>(
                SEARCH_RESOURCE,
                crate::i18n::t("session-search"),
            )
            .into_resource(),
            tilo_resource::LiveRow::typed::<LaunchpadRoot>(
                LAUNCHPAD_RESOURCE,
                crate::i18n::t("session-launchpad"),
            )
            .into_resource(),
            tilo_resource::LiveRow::typed::<WallpaperRoot>(
                WALLPAPER_RESOURCE,
                crate::i18n::t("session-wallpaper"),
            )
            .into_resource(),
            offer::<SaidEffect>(
                tilo_resource::LiveRow::typed::<SaidRoot>(
                    NOTICE_RESOURCE,
                    crate::i18n::t("session-notice"),
                )
                .into_resource(),
            ),
            offer::<crate::live_list::LiveEffect>(
                tilo_resource::LiveRow::typed::<LiveRoot>(
                    crate::live_list::RESOURCE,
                    crate::i18n::t("session-live"),
                )
                .into_resource(),
            ),
        ]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        self.list(resource)
    }

    fn poll(&mut self) -> bool {
        let now = self.lists_key();
        let notice = self.handle.notice().borrow().get().cloned();
        if self.lists.as_ref() == Some(&now) && self.notice_seen == notice {
            return false;
        }
        self.lists = Some(now);
        self.notice_seen = notice;
        true
    }

    /// The instant the switcher's panel comes up while Alt is held, or a passing notice runs out:
    /// what the shell redraws for without a frame in between.
    fn wakes_in(&self) -> Option<std::time::Duration> {
        let soonest = [
            self.next_deadline(),
            self.handle.notice().borrow().deadline(),
        ]
        .into_iter()
        .flatten()
        .min();
        soonest.map(|at| at.saturating_duration_since(tilo_clock::now()))
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        match resource {
            crate::live_list::RESOURCE => {
                let effect = decode::<crate::live_list::LiveEffect>(
                    &self.resources(),
                    resource,
                    action,
                    arguments,
                )?;
                self.live.act(effect)
            }
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
                }
            }
            INVITE_RESOURCE => {
                let effect =
                    decode::<InviteEffect>(&self.resources(), resource, action, arguments)?;
                self.invite(&effect)
            }
            NOTICE_RESOURCE => {
                let effect = decode::<SaidEffect>(&self.resources(), resource, action, arguments)?;
                match effect {
                    SaidEffect::PressNotice { action } => {
                        if !self.handle.press_notice(&action) {
                            return Err(ApiError::InvalidArguments(format!(
                                "the notice offers no {action}"
                            )));
                        }
                    }
                    SaidEffect::Hide => {
                        self.handle.notice().borrow_mut().dismiss();
                    }
                }
                Ok(Started::Done(tilo_actions::ActionOutput::default()))
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
        let handle = DesktopHandle::new(tree.clone(), 5);
        assert!(!windows(&tree, &handle)[0].not_responding);
        assert!(handle.set_unresponsive_windows(vec![77]));
        assert!(windows(&tree, &handle)[0].not_responding);
        // Another window's silence is not this one's.
        assert!(handle.set_unresponsive_windows(vec![78]));
        assert!(!windows(&tree, &handle)[0].not_responding);
    }

    fn surfaces() -> Surfaces {
        Surfaces {
            overlays: Overlays::new(),
            notices: tilo_session_core::desktop::state::DesktopNotifications::default(),
            overview: Rc::new(std::cell::Cell::new(false)),
            handle: DesktopHandle::new(Layout::default(), 1),
            emoji: Rc::new(std::cell::Cell::new(false)),
            about: Rc::new(std::cell::Cell::new(false)),
            power: Rc::new(std::cell::Cell::new(false)),
        }
    }

    #[test]
    fn the_overview_turns_the_shells_reading_with_the_surface() {
        let surfaces = surfaces();
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
        let surfaces = surfaces();
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
        let surfaces = surfaces();
        surfaces.open(crate::quick_settings::SURFACE);
        surfaces.open(Overlay::Centre.name());
        assert_eq!(surfaces.overlays.here(), Overlay::Centre);
    }

    /// The shell draws the one centre the bar's bell counts: a press on it opens the shell's own,
    /// and asks no other chrome for one.
    #[test]
    fn the_shells_own_centre_is_opened_where_the_shell_draws_it() {
        let screen = surfaces();
        screen.toggle(Overlay::Centre.name());
        assert_eq!(screen.overlays.here(), Overlay::Centre);
        assert_eq!(screen.notices.take_centre_ask(), None);
    }

    #[test]
    fn a_shell_draws_the_quick_settings_it_shows() {
        let surfaces = surfaces();
        surfaces.toggle(crate::quick_settings::SURFACE);
        assert_eq!(surfaces.overlays.here(), Overlay::QuickSettings);
        assert!(!surfaces.overlays.is_open(Part::QuickSettings));
        surfaces.close(crate::quick_settings::SURFACE);
        assert_eq!(surfaces.overlays.here(), Overlay::Rest);
    }
}
