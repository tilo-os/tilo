//! This device's lists as resources of the session's: its windows, the tray, the agents at the
//! person's door and the desk styles, and inviting into a workspace. A shell's views read them, and
//! so does a script or an agent, through the door, by the same addresses: one reading for every
//! reader.

use std::rc::Rc;

use serde_json::{Map, Value};
use tilo_actions::remote::session_actions::INVITE_RESOURCE;
use tilo_actions::{ActionAddress, DoorError, ResourceSpec};
use tilo_desktop::chrome::ChromeIntent;
use tilo_desktop::desktop_host::DesktopAsk;
use tilo_desktop::overlays::{Overlay, Overlays, Slot};
use tilo_desktop_model::{Layout, Workspace, WorkspaceId};
use tilo_desktop_surface::DesktopHandle;
use tilo_desktop_surface::navigation::ChromeEffect;
use tilo_peer_state::join::Role;
use tilo_peer_state::join::invitations::PendingInvitation;
use tilo_resource::Effect;
use tilo_service::{Service, Started, decode, offer, reading};

use crate::desk_windows::{reaching, windows};

/// What the device's lists are made from ([`Desk::lists_key`]), one entry per resource: the
/// windows, the tray, the agents and the styles. An entry that moved is a reading that moved.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ListsKey([(u64, u64, u64, bool); 4]);

/// The desk's windows, every workspace's: what the switcher, the dock and the launcher list.
pub(crate) const WINDOWS_RESOURCE: &str = "session:windows";
/// The running applications' tray items.
pub(crate) const TRAY_RESOURCE: &str = "session:tray";
/// The agents the person admitted at their agent door.
pub(crate) const AGENTS_RESOURCE: &str = "session:agents";
/// The desk styles every installed package offers.
pub(crate) const STYLES_RESOURCE: &str = "session:styles";

/// The windows as a view of another language reads them (`device.windows`).
#[must_use]
pub fn windows_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<crate::desk_windows::Windows, tilo_resource::NoEffect>(
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
    tilo_resource::surface::reading_surface::<crate::agent_door::Agents, AgentsEffect>(
        "agents", "Agents",
    )
}

/// The desk styles as a view of another language reads them (`device.styles`).
#[must_use]
pub fn styles_surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<OfferedStyles, tilo_resource::NoEffect>(
        "styles", "Styles",
    )
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
}

/// The desk styles every installed package offers (shell.md §6).
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
    pub offered: Vec<crate::desk_styles::DeskStyle>,
}

/// What the tray offers.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.session.tray")]
pub(crate) enum TrayEffect {
    /// Open or close the running application's tray item `key`: its window, or its menu when the
    /// item is one
    #[effect(row, label = "session-tray-open")]
    OpenTrayItem { key: String },
}

/// What the agents offer.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.session.agents")]
pub(crate) enum AgentsEffect {
    /// End the agent `agent`'s work here: the connection the person admitted closes and its
    /// presence leaves the bar
    #[effect(label = "session-agents-stop")]
    StopAgent {
        #[param("The agent, by the key its presence names it")]
        agent: String,
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
    Invitations {
        /// The workspace, by its key; the one in front when absent
        #[param("The workspace, by its key; the one in front when absent")]
        workspace: Option<String>,
    },
    /// Make an invitation into a workspace from this device: the link to hand to whoever is
    /// being invited, which their device joins with
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
    ShowInvite {
        /// The workspace, by its key; the one in front when absent
        #[param("The workspace, by its key; the one in front when absent")]
        workspace: Option<String>,
    },
    /// Show how an agent or another computer connects as the person: the setup their own card's
    /// "Connect an agent or computer" opens. An app that lists what acts as them offers adding to
    /// it this way.
    ShowConnect,
    /// Open the Add a device sheet: the link a new device of the person's becomes one of theirs,
    /// shown as a code its camera reads. An app that lists what acts as the person offers adding
    /// to it this way.
    ShowPairDevice,
    /// Cancel the invitation `invite` of a workspace: its link admits nobody from now on
    CancelInvitation {
        /// The invitation's id, 64 hex characters, as listing invitations reports it
        #[param("The invitation's id, 64 hex characters, as listing invitations reports it")]
        invite: String,
        /// The workspace, by its key; the one in front when absent
        #[param("The workspace, by its key; the one in front when absent")]
        workspace: Option<String>,
    },
}

/// The surfaces the desk shows over its windows, and where each stands: the shell's own views,
/// except the quick settings, which stand in the panel's own app where the device has one and
/// no shell covers the desk that app stands in.
/// What `show-surface` and `close-surface` open and close, whether the person asks on the
/// desk's door or on the chrome's above it, so both open the one surface.
#[derive(Clone)]
pub(crate) struct Surfaces {
    /// Whether the device has the quick settings' app; without, the status opens the shell's own
    /// view of them.
    pub(crate) status_panel: bool,
    /// Whether the shell draws the whole screen over the desk: the desk draws none of the panel's
    /// app there, so the quick settings are the shell's own view, as every other surface is.
    pub(crate) shell: bool,
    /// The stack the panel's own app is placed in, with the shell's surface up.
    pub(crate) overlays: Overlays,
    /// The notifications the desk's own chrome counts and draws: where the one centre opens
    /// when the desk draws it, as the bar's bell opens it.
    pub(crate) notices: tilo_desktop_surface::DesktopNotifications,
    /// Whether the overview is the surface up, as the desk's own widgets fold behind it and as
    /// whoever asks for the desk's state reads it. It turns with the surface itself, so a person's
    /// press answers at once rather than a frame later.
    pub(crate) overview: Rc<std::cell::Cell<bool>>,
}

impl Surfaces {
    /// Whether the quick settings stand in the panel's own app: the device has it, and the desk
    /// draws the app it stands in.
    pub(crate) fn panel_shows(&self, surface: &str) -> bool {
        surface == crate::quick_settings::SURFACE && self.status_panel && !self.shell
    }

    /// Whether the desk's own chrome draws the centre, rather than the shell's view of it: the
    /// desk draws it whenever it draws the screen, and the bell counts the same one.
    fn desk_centre(&self, surface: &str) -> bool {
        surface == Overlay::Centre.name() && !self.shell
    }

    /// Show `surface` (`show-surface`): the one up closes, and any other opens.
    pub(crate) fn toggle(&self, surface: &str) {
        self.caught_up();
        if self.desk_centre(surface) {
            // One centre: the desk's chrome draws the one the bar's bell counts, and it is asked
            // to open or shut as the bell asks it.
            self.notices.ask_centre(!self.notices.centre_open());
            self.panel_closes();
            self.settled();
            return;
        }
        // The quick settings are their own app, standing where the stack places its slot: a
        // press opens the slot, and the panel's app draws it there. A device without that app,
        // or one the shell covers, keeps the shell's own view of the surface.
        if self.panel_shows(surface) {
            if self.overlays.is_open(Slot::QuickSettings) {
                self.overlays.close(Slot::QuickSettings);
            } else {
                self.clear_shell();
                self.overlays.open(Slot::QuickSettings, None);
            }
            return;
        }
        self.panel_closes();
        if let Some(surface) = Overlay::named(surface) {
            if self.overlays.here() == surface {
                self.overlays.show(Overlay::Rest);
            } else {
                self.overlays.show(surface);
            }
        }
        self.clear_beside();
        self.settled();
    }

    /// Show the surface `name` names (`show-surface`), whether or not it is up.
    pub(crate) fn open(&self, name: &str) {
        self.caught_up();
        if self.desk_centre(name) {
            self.notices.ask_centre(true);
            self.panel_closes();
            self.settled();
            return;
        }
        if self.panel_shows(name) {
            self.clear_shell();
            self.overlays.open(Slot::QuickSettings, None);
            return;
        }
        self.panel_closes();
        if let Some(surface) = Overlay::named(name) {
            self.overlays.show(surface);
        }
        self.clear_beside();
        self.settled();
    }

    /// One surface at a time: the desk's own popovers go down under the shell's view of the surface
    /// just opened.
    fn clear_beside(&self) {
        if self.overlays.here() == Overlay::Rest {
            return;
        }
        self.clear_panel();
    }

    /// Take every popover of the desk's own down: a surface outside its stack is coming up instead.
    pub(crate) fn clear_panel(&self) {
        for slot in Slot::ALL {
            if slot.stands_beside() {
                self.overlays.close(slot);
            }
        }
    }

    /// One surface at a time: the shell's view goes down under the desk's own popover or the
    /// panel's app.
    pub(crate) fn clear_shell(&self) {
        self.caught_up();
        if self.overlays.here() == Overlay::Rest {
            return;
        }
        self.overlays.show(Overlay::Rest);
        self.settled();
    }

    /// Whether any of the desk's own popovers stands over the windows now.
    pub(crate) fn beside_up(&self) -> bool {
        Slot::ALL
            .into_iter()
            .filter(|slot| !self.drawn_by_the_shell(*slot))
            .any(|slot| slot.stands_beside() && self.overlays.is_open(slot))
    }

    /// Whether the shell's own view draws `slot` instead of the desk, so what the desk keeps in
    /// that slot stands over nothing: the shell's launcher is the search under the shell's screen,
    /// as the quick settings are the shell's view of them. A popover nobody draws is not a surface
    /// the person is looking at, so it neither takes the shell's surface down nor keeps its keys.
    fn drawn_by_the_shell(&self, slot: Slot) -> bool {
        self.shell && slot == Slot::Search
    }

    /// Close `surface` (`close-surface`), when it is the one up.
    pub(crate) fn close(&self, surface: &str) {
        self.caught_up();
        if self.desk_centre(surface) {
            self.notices.ask_centre(false);
            return;
        }
        if self.panel_shows(surface) {
            self.overlays.close(Slot::QuickSettings);
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
        if self.overlays.is_open(Slot::QuickSettings) {
            self.overlays.close(Slot::QuickSettings);
        }
    }

    /// Read the overview as the flag has it before a surface turns: the desk's own chrome opens and
    /// closes it too (its action, its keys, Escape), and a press answers what the person sees.
    fn caught_up(&self) {
        let here = self.overlays.here() == Overlay::Overview;
        if self.overview.get() && !here {
            self.overlays.show(Overlay::Overview);
        } else if !self.overview.get() && here {
            self.overlays.show(Overlay::Rest);
        }
    }

    /// Keep the desk's reading of the overview in step with the surface just changed: the desk
    /// folds behind it and its state names it as up the moment the person's press opens it,
    /// instead of after the shell's next frame.
    fn settled(&self) {
        self.overview.set(self.overlays.here() == Overlay::Overview);
    }
}

/// The desk styles every installed package offers, with the revision that moves when the
/// session re-reads its store: a package installed or taken away moves the desk's reading.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Styles {
    pub(crate) revision: u64,
    pub(crate) list: Vec<crate::desk_styles::DeskStyle>,
    /// The store's registry the list was read at: a store that reads the same is not read again,
    /// and what the log says of it is said only when it changes.
    pub(crate) registry: Option<crate::desk_styles::Registry>,
}

/// The desk as the session reads it. See the module docs.
pub(crate) struct Desk {
    pub(crate) handle: DesktopHandle,
    pub(crate) person: String,
    /// The surfaces the desk shows over its windows, and where they stand.
    pub(crate) surfaces: Surfaces,
    /// Who is here, when this session takes part in the collaboration plane.
    pub(crate) sync: Option<Rc<crate::sync::Client>>,
    /// The apps' live activities: Clocks' timer is the bar's clock beside the time, and the person
    /// putting the floating one away hides it until Clocks shows something new.
    pub(crate) activities: Rc<crate::chrome::Activities>,
    /// The running applications' tray items, when their watcher serves: the bar shows them,
    /// and the person's clicks go back through it.
    pub(crate) tray: Option<std::sync::Arc<tilo_tray_host::Tray>>,
    /// The agents the person admitted at its agent door: each works here as itself, and the bar
    /// shows it as its own presence.
    pub(crate) seats: std::sync::Arc<crate::agent_door::Seats>,
    /// This device's workspaces' record, when the session holds one: the invitations a workspace's
    /// people made are kept there, by the device that made them.
    pub(crate) engine: Option<std::sync::Arc<crate::engine::Engine>>,
    /// What the desk asks the session for while it runs: opening the invite sheet, which is the
    /// session's own surface and not a layer the desk draws.
    pub(crate) asks: Rc<std::cell::RefCell<Vec<tilo_desktop::desktop_host::DesktopAsk>>>,
    /// The desk styles every installed package offers, as `device.styles` reads them
    /// (shell.md §6): what the person's switch offers. The session fills it from its package
    /// store, and moves it when a package is installed or taken away.
    pub(crate) styles: std::rc::Rc<std::cell::RefCell<Styles>>,
    /// What the lists were last published from.
    pub(crate) lists: Option<ListsKey>,
}

/// The pieces a session's desk is made of, once: the same seats, surfaces and records reach the
/// desk's reading and every part of the shell.
pub(crate) struct Shared {
    pub(crate) seats: std::sync::Arc<crate::agent_door::Seats>,
    pub(crate) handle: DesktopHandle,
    pub(crate) person: String,
    pub(crate) activities: Rc<crate::chrome::Activities>,
    pub(crate) surfaces: Surfaces,
    pub(crate) sync: Option<Rc<crate::sync::Client>>,
    pub(crate) tray: Option<std::sync::Arc<tilo_tray_host::Tray>>,
    pub(crate) engine: Option<std::sync::Arc<crate::engine::Engine>>,
    pub(crate) asks: Rc<std::cell::RefCell<Vec<tilo_desktop::desktop_host::DesktopAsk>>>,
}

/// A factory making the session's desk, its styles read from the person's store as they move
/// (shell.md §6): called once for the desk's own service and once for the shell, each a view of
/// the same seats, surfaces and records.
pub(crate) fn desk_from(shared: Shared) -> impl Fn() -> Desk {
    let styles = Rc::new(std::cell::RefCell::new(Styles::default()));
    move || Desk {
        seats: std::sync::Arc::clone(&shared.seats),
        handle: shared.handle.clone(),
        person: shared.person.clone(),
        activities: Rc::clone(&shared.activities),
        surfaces: shared.surfaces.clone(),
        sync: shared.sync.clone(),
        tray: shared.tray.clone(),
        engine: shared.engine.clone(),
        asks: Rc::clone(&shared.asks),
        styles: Rc::clone(&styles),
        lists: None,
    }
}

impl Desk {
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
            (self.seats.revision(), 0, 0, false),
            (self.styles.borrow().revision, 0, 0, false),
        ])
    }

    /// The reading of one of the lists, by its resource.
    fn list(&self, resource: &str) -> Option<Value> {
        match resource {
            WINDOWS_RESOURCE => Some(reading(&crate::desk_windows::Windows {
                all: self
                    .handle
                    .read(|tree| self.windows_reaching(tree, tilo_clock::now())),
            })),
            TRAY_RESOURCE => Some(reading(&Tray {
                items: self.tray_items(),
            })),
            AGENTS_RESOURCE => Some(reading(&crate::agent_door::Agents {
                seated: self.seats.readings(),
            })),
            STYLES_RESOURCE => Some(reading(&OfferedStyles {
                offered: self.styles.borrow().list.clone(),
            })),
            _ => None,
        }
    }

    /// When the desk has something to draw that it has not drawn yet, without a frame in between:
    /// the instant the switcher's panel comes up while Alt is held on it.
    pub(crate) fn next_deadline(&self) -> Option<tilo_clock::time::Instant> {
        self.surfaces.overlays.deadline(tilo_clock::now())
    }

    /// The desk's windows, with the one the switcher's walk has reached marked focused while its
    /// panel shows: the tile its walk chose is the one lifted. A quick flick shows no panel, so it
    /// lifts nothing either.
    fn windows_reaching(
        &self,
        tree: &Layout,
        now: tilo_clock::time::Instant,
    ) -> Vec<crate::desk_windows::Window> {
        let mut windows = windows(tree, &self.handle);
        // The app as people know it, never the id: a window is named by what it shows, and its
        // `app_title` is a person's own name for the window, not its app's name.
        for window in &mut windows {
            window.origin_app = crate::app_names::app_name(&window.app_key);
        }
        if self.surfaces.overlays.revealed(now)
            && let Some(at) = reaching(&self.surfaces.overlays, &windows)
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
    /// own words for it, and whether choosing it opens a menu rather than its window.
    fn tray_items(&self) -> Vec<TrayItem> {
        self.tray.as_ref().map_or_else(Vec::new, |tray| {
            tray.snapshot()
                .into_iter()
                .map(|item| TrayItem {
                    key: item.key,
                    title: if item.title.trim().is_empty() {
                        item.id
                    } else {
                        item.title
                    },
                    menu: item.item_is_menu,
                })
                .collect()
        })
    }
}

impl Desk {
    /// The id of the desk's workspace `key`, or of the workspace in front when the caller names
    /// none.
    fn workspace_id(&self, workspace: Option<&str>) -> Result<WorkspaceId, DoorError> {
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
                DoorError::InvalidArguments(format!("there is no workspace {key} on this device"))
            }
            None => DoorError::Failed("no workspace is in front".to_owned()),
        })
    }

    /// The invitations into a workspace this device's person made that are still redeemable, as the
    /// workspace's host lists them: its id in hex, the part it grants, whom it is addressed to, how
    /// it names them and when it lapses.
    fn invitations(&self, workspace: Option<&str>) -> Result<Started, DoorError> {
        let engine = self.engine()?;
        let workspace_id = self.workspace_id(workspace)?;
        let invitations = engine
            .invitations(&workspace_id.to_string(), std::time::SystemTime::now())
            .map_err(DoorError::Failed)?;
        let value = Value::Array(invitations.iter().map(invitation_json).collect());
        Ok(Started::Done(tilo_actions::ActionOutput {
            value: Some(value),
            resources: Vec::new(),
        }))
    }

    /// Cancel one of them, from the device that issued it: the link admits nobody from now on.
    fn cancel_invitation(
        &self,
        invite: &str,
        workspace: Option<&str>,
    ) -> Result<Started, DoorError> {
        let engine = self.engine()?;
        let workspace_id = self.workspace_id(workspace)?;
        let cancelled = engine
            .cancel_invitation(
                &workspace_id.to_string(),
                invite,
                std::time::SystemTime::now(),
            )
            .map_err(DoorError::Failed)?;
        Ok(Started::Done(tilo_actions::ActionOutput {
            value: Some(serde_json::json!({"cancelled": cancelled})),
            resources: Vec::new(),
        }))
    }

    /// Hand out a link into a workspace, made by this device's own person: the invitation the desk
    /// lists from now on, in the text another device joins with.
    fn issue_invitation(
        &self,
        role: Option<&str>,
        invitee_name: Option<String>,
        recipient: Option<&str>,
        expires_in_seconds: Option<u64>,
        workspace: Option<&str>,
    ) -> Result<Started, DoorError> {
        let engine = self.engine()?;
        let room = self.workspace_id(workspace)?;
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
                        DoorError::InvalidArguments(format!("`{name}` is no part of a workspace"))
                    })?,
                false,
            ),
        };
        let recipient = recipient
            .map(|person| {
                let not_a_key = || {
                    DoorError::InvalidArguments(format!(
                        "`{person}` is not a person's key, 64 hex characters"
                    ))
                };
                let key = hex::decode(person).map_err(|_| not_a_key())?;
                <[u8; 32]>::try_from(key.as_slice()).map_err(|_| not_a_key())
            })
            .transpose()?;
        let invitee_name = match (invitee_name, recipient) {
            (None, Some(recipient)) => engine.name_of(&room.to_string(), recipient),
            (name, _) => name,
        };
        // The link names this device, which answers whoever redeems it: a session taking part in no
        // transport has no address to name, and one that has says where it is reached through its
        // relay, so the link works from behind any network ([`crate::sync::Client::invitation_addr`]).
        let endpoint = self
            .sync
            .as_ref()
            .map(|sync| sync.invitation_addr())
            .ok_or_else(|| {
                DoorError::Failed(
                    "this session takes part in no transport, so no link can name it".to_owned(),
                )
            })?;
        // The link is signed by this device's signer, which may answer only later: the asker hears
        // it once signed, and the loop goes on meanwhile.
        let engine = std::sync::Arc::clone(engine);
        let room = room.to_string();
        let expires_in = expires_in_seconds.unwrap_or(DEFAULT_INVITE_SECONDS);
        Ok(Started::Later(crate::landing::off_loop(async move {
            let issued = engine
                .issue_invitation(
                    &room,
                    tilo_session_client::engine::Part {
                        role,
                        administrator: makes_administrator,
                    },
                    invitee_name,
                    recipient,
                    expires_in,
                    &endpoint,
                    std::time::SystemTime::now(),
                )
                .await
                .map_err(DoorError::Failed)?;
            Ok(tilo_actions::ActionOutput {
                value: Some(serde_json::json!({
                    "invite": issued.invite,
                    "link": issued.link,
                    "role": issued.role.key(),
                    "administrator": makes_administrator,
                    "expires_at_ns": issued.expires_at_ns,
                })),
                resources: Vec::new(),
            })
        })))
    }

    /// What an invitation verb answers, when `effect` is one of them: the links that still work,
    /// one made, or one taken back.
    fn invitation(&self, effect: &InviteEffect) -> Option<Result<Started, DoorError>> {
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

    /// This device's engine: invitations are its record's, and a session without one has none.
    fn engine(&self) -> Result<&std::sync::Arc<crate::engine::Engine>, DoorError> {
        self.engine
            .as_ref()
            .ok_or_else(|| DoorError::Failed("this session holds no workspace's record".to_owned()))
    }
}

impl Desk {
    /// Open the invite sheet on a workspace: the workspace comes to the front, and the sheet the
    /// bar's button opens is shown on it, through the session, which holds the surface.
    fn show_invite(&self, workspace: Option<&str>) -> Result<Started, DoorError> {
        let workspace = match workspace {
            Some(key) => key.to_owned(),
            None => self
                .handle
                .read(|tree| {
                    tree.active_workspace()
                        .map(|active| active.id().to_string())
                })
                .ok_or_else(|| DoorError::Failed("no workspace is in front".to_owned()))?,
        };
        let known = self.handle.read(|tree| {
            tree.workspaces
                .iter()
                .any(|candidate| candidate.id().to_string() == workspace)
        });
        if !known {
            return Err(DoorError::InvalidArguments(format!(
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
                .map_err(DoorError::Failed)?;
        }
        self.asks
            .borrow_mut()
            .push(DesktopAsk::Intent(ChromeIntent::Invite));
        Ok(Started::Done(tilo_actions::ActionOutput::default()))
    }
}

/// A pending invitation as listing invitations reports it, in the workspace host's own words: a
/// client that lists them from a host reads the same fields.
fn invitation_json(invitation: &PendingInvitation) -> Value {
    serde_json::json!({
        "invite": hex::encode(invitation.invite_id),
        "role": invitation.role.key(),
        "recipient": invitation.recipient_user_public_key.map(hex::encode),
        "invitee_name": invitation.invitee_name,
        "expires_at_ns": invitation.expires_at_ns,
    })
}

/// How long an invitation the desk hands out without being told otherwise works for: an hour, as a
/// person's invitation does.
const DEFAULT_INVITE_SECONDS: u64 = 3_600;

impl Desk {
    /// Open or close the running application's tray item `key`, as a press on its icon does: its
    /// window, or its menu when the item is one.
    fn open_tray_item(&self, key: &str) -> Result<Started, DoorError> {
        let Some(tray) = &self.tray else {
            return Err(DoorError::Failed(
                "no tray is served: the press reaches no application".to_owned(),
            ));
        };
        let Some(item) = tray.snapshot().into_iter().find(|item| item.key == key) else {
            return Err(DoorError::InvalidArguments(format!(
                "there is no tray item {key}"
            )));
        };
        let request = if item.item_is_menu {
            tilo_tray_host::TrayRequest::ContextMenu { x: 0, y: 0 }
        } else {
            tilo_tray_host::TrayRequest::Activate { x: 0, y: 0 }
        };
        tray.request_or_else(key, request, |_| {})
            .map_err(DoorError::Failed)?;
        Ok(Started::Done(tilo_actions::ActionOutput::default()))
    }
}

impl Desk {
    /// Carry out `effect` of the inviting resource.
    fn invite(&self, effect: &InviteEffect) -> Result<Started, DoorError> {
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

impl Service for Desk {
    fn resources(&self) -> Vec<ResourceSpec> {
        let kind = tilo_action_policy::roles::SESSION_KIND;
        vec![
            ResourceSpec::new(WINDOWS_RESOURCE, kind, crate::i18n::t("session-windows")),
            offer::<TrayEffect>(ResourceSpec::new(
                TRAY_RESOURCE,
                kind,
                crate::i18n::t("session-tray"),
            )),
            offer::<AgentsEffect>(ResourceSpec::new(
                AGENTS_RESOURCE,
                kind,
                crate::i18n::t("session-agents"),
            )),
            offer::<InviteEffect>(ResourceSpec::new(
                INVITE_RESOURCE,
                kind,
                crate::i18n::t("session-invite"),
            )),
            ResourceSpec::new(STYLES_RESOURCE, kind, crate::i18n::t("session-styles")),
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
    ) -> Result<Started, DoorError> {
        match resource {
            TRAY_RESOURCE => {
                let TrayEffect::OpenTrayItem { key } =
                    decode::<TrayEffect>(&self.resources(), resource, action, arguments)?;
                self.open_tray_item(&key)
            }
            AGENTS_RESOURCE => {
                let AgentsEffect::StopAgent { agent } =
                    decode::<AgentsEffect>(&self.resources(), resource, action, arguments)?;
                if !self.seats.stop(&agent) {
                    return Err(DoorError::InvalidArguments(format!(
                        "no agent {agent} works here"
                    )));
                }
                Ok(Started::Done(tilo_actions::ActionOutput::default()))
            }
            INVITE_RESOURCE => {
                let effect =
                    decode::<InviteEffect>(&self.resources(), resource, action, arguments)?;
                self.invite(&effect)
            }
            _ => Err(DoorError::Unknown(ActionAddress::new(resource, action))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window whose app stopped answering says so in the reading every view of the desk reads,
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
        let tree = tilo_desktop_surface::seed_tree(&mut minter, "Desk", vec![tab]);
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
        Surfaces {
            status_panel,
            shell,
            overlays: Overlays::new(),
            notices: tilo_desktop_surface::DesktopNotifications::default(),
            overview: Rc::new(std::cell::Cell::new(false)),
        }
    }

    #[test]
    fn the_overview_turns_the_desks_reading_with_the_surface() {
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

    /// The desk's chrome opens and closes the overview itself as well: a press then reads the
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

    /// The desk draws the one centre the bar's bell counts: a press on it asks the desk's own
    /// chrome, as the bell asks it, and puts the panel that asked away.
    #[test]
    fn the_desks_own_centre_is_opened_where_the_desk_draws_it() {
        let surfaces = surfaces(true, false);
        surfaces.open(crate::quick_settings::SURFACE);
        assert!(surfaces.overlays.is_open(Slot::QuickSettings));
        surfaces.toggle(Overlay::Centre.name());
        assert_eq!(
            surfaces.notices.take_centre_ask(),
            Some(true),
            "the desk's chrome is asked to open the centre"
        );
        assert_eq!(
            surfaces.overlays.here(),
            Overlay::Rest,
            "the shell draws no centre of its own over a desk that draws one"
        );
        assert!(
            !surfaces.overlays.is_open(Slot::QuickSettings),
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
            assert!(!surfaces.overlays.is_open(Slot::QuickSettings));
            surfaces.close(crate::quick_settings::SURFACE);
            assert_eq!(surfaces.overlays.here(), Overlay::Rest);
        }
    }

    #[test]
    fn the_desk_leaves_the_quick_settings_to_the_panels_app_it_has() {
        let surfaces = surfaces(true, false);
        surfaces.toggle(crate::quick_settings::SURFACE);
        assert!(surfaces.overlays.is_open(Slot::QuickSettings));
        assert_eq!(surfaces.overlays.here(), Overlay::Rest);
        surfaces.close(crate::quick_settings::SURFACE);
        assert!(!surfaces.overlays.is_open(Slot::QuickSettings));
    }

    #[test]
    fn without_the_panels_app_the_desk_shows_the_shells_quick_settings() {
        let surfaces = surfaces(false, false);
        surfaces.open(crate::quick_settings::SURFACE);
        assert_eq!(surfaces.overlays.here(), Overlay::QuickSettings);
        assert!(!surfaces.overlays.is_open(Slot::QuickSettings));
    }
}
