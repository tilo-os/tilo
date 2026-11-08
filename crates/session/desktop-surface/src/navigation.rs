//! `DesktopNavigationV1`: the versioned per-surface navigation contract a
//! host page consumes. Routes name a workspace, strip, window group and tab
//! of the desktop model. Identities are decimal strings on the wire so a
//! full `u64` never passes through a JavaScript number.
//!
//! Wire types ported from the qOS browser runtime (PR 541,
//! `desktop_navigation.rs`) onto `tilo-desktop-model`.
use crate::i18n::t;
use serde::{Deserialize, Serialize};
use tilo_desktop_model::{
    DesktopTree, GroupId, StripId, TabId, WindowGroup, Workspace, WorkspaceSlot,
};
use tilo_resource::Effect;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Route {
    pub workspace: String,
    pub strip: String,
    pub group: String,
    pub tab: String,
}

/// A route resolved against the model's id types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RouteIds {
    pub workspace: WorkspaceSlot,
    pub strip: StripId,
    pub group: GroupId,
    pub tab: TabId,
}

fn id(value: &str) -> Result<u64, String> {
    value
        .parse::<u64>()
        .ok()
        .filter(|id| id.to_string() == value)
        .ok_or_else(|| "Invalid navigation identity".into())
}

impl Route {
    /// # Errors
    /// Refuses an identity that is not a canonical decimal `u64`.
    pub fn ids(&self) -> Result<RouteIds, String> {
        Ok(RouteIds {
            workspace: WorkspaceSlot(id(&self.workspace)?),
            strip: StripId(id(&self.strip)?),
            group: id(&self.group)?,
            tab: id(&self.tab)?,
        })
    }

    pub fn from_ids(ids: RouteIds) -> Self {
        Self {
            workspace: ids.workspace.0.to_string(),
            strip: ids.strip.0.to_string(),
            group: ids.group.to_string(),
            tab: ids.tab.to_string(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ActivityView {
    CatchUp,
    Explore,
    Replay,
}

impl ActivityView {
    /// Why a surface with `capabilities` cannot present `self`, if it
    /// cannot. The views read retained record and desktop history, which
    /// only a host that installed a history source holds.
    pub fn unavailable(self, capabilities: &Capabilities) -> Option<&'static str> {
        match self {
            Self::CatchUp | Self::Explore if capabilities.workspace_history => None,
            Self::Replay if capabilities.workspace_replay => None,
            Self::CatchUp => {
                Some("Catch up needs retained record history, which this desktop does not carry")
            }
            Self::Explore => {
                Some("Explore needs retained record history, which this desktop does not carry")
            }
            Self::Replay => {
                Some("Replay needs retained desktop history, which this desktop does not carry")
            }
        }
    }
}

/// A per-window history move: the `target` of a `history` command.
pub type HistoryDirection = ui::gesture::NavDirection;

fn history_direction(target: &str) -> Result<HistoryDirection, String> {
    match target {
        "back" => Ok(HistoryDirection::Back),
        "forward" => Ok(HistoryDirection::Forward),
        _ => Err("Unknown history target".into()),
    }
}

/// Where a window goes: another strip of its workspace, another workspace,
/// or another display. Exactly one of them names the place; `before` and
/// `group` say where in a strip it lands.
#[derive(
    Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq, tilo_resource::JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct Destination {
    /// The strip of the window's workspace to carry the window to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strip: Option<String>,
    /// The workspace to take the window to, as a window of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// The display to show the window on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
    /// The window group of `strip` to stand before; the strip's end when
    /// absent, which in the window's own strip moves it to the last place.
    /// Any place in the strip it already stands on reorders it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    /// The window group of the tab's own strip to put the tab in, its tab
    /// joining that window instead of a window of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

/// A share of its strip a window takes in one step.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum WidthPreset {
    /// Half the strip.
    Half,
    Narrow,
    Normal,
    Wide,
}

impl From<WidthPreset> for tilo_desktop_model::WindowWidth {
    fn from(preset: WidthPreset) -> Self {
        use tilo_desktop_model::WindowWidth;
        match preset {
            WidthPreset::Half => WindowWidth::Fraction(500),
            WidthPreset::Narrow => WindowWidth::Narrow,
            WidthPreset::Normal => WindowWidth::Normal,
            WidthPreset::Wide => WindowWidth::Wide,
        }
    }
}

/// What the desk under the windows can be asked to do: which window has
/// focus, closing one, which display is worked on, what the search and the
/// empty desk's launchpad do. Each is one published action on the desk's
/// resource, so a person's chord, a page, a script and an agent do the same
/// thing by the same verb. Ids travel as the decimal text the desk
/// addresses its windows by; a caller that names something the desk does
/// not hold is refused rather than given the nearest thing.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "so.tilo.desk")]
pub enum DeskEffect {
    /// Bring its window to the front
    #[effect(own_device, idempotent)]
    Focus {
        /// The tab to bring forward.
        #[param(decimal, "The tab to focus")]
        tab: String,
    },

    /// Close its window
    #[effect(own_device)]
    CloseTab {
        /// The tab's id.
        #[param(decimal, "The tab to close")]
        tab: String,
    },

    /// Step through the windows
    ///
    /// Along the shown strip, onto the neighbouring display past either end.
    #[effect(own_device)]
    StepFocus {
        /// How many windows on; negative is back.
        #[param("How many windows on; negative goes back")]
        steps: i64,
    },

    /// Focus a display
    #[effect(own_device, idempotent)]
    FocusDisplay {
        /// The display's id.
        #[param(decimal, "The display to focus")]
        display: String,
    },

    /// Move the window in front to another display
    #[effect(own_device)]
    MoveToDisplay {
        /// How many displays on; negative is back.
        #[param("How many displays on; negative goes back")]
        steps: i64,
    },

    /// Search the desktop
    ///
    /// Open the desktop's search, or close it when it is already open, as
    /// its key does; with `query` typed when given, into the search already
    /// open instead.
    #[effect(own_device)]
    Search {
        /// What to type.
        #[param("The query to type")]
        query: Option<String>,
    },

    /// Choose a search row
    #[effect(own_device)]
    ChooseSearchRow {
        /// The row, counting from 0 as the reading lists them.
        #[param("The row to choose")]
        row: u64,
        /// The line its action asks for.
        #[param("The line the row's action asks for")]
        argument: Option<String>,
    },

    /// Choose a launchpad card
    #[effect(own_device)]
    Launchpad {
        /// The card's slug, or `apps` for the applications.
        #[param("The card's slug, or \"apps\" for the applications")]
        card: String,
    },

    /// Keep waiting for its app
    ///
    /// Give the app of a window that stopped answering its pings another
    /// period to answer before the window says so again.
    #[effect(own_device, idempotent)]
    WaitWindow {
        /// The tab's id.
        #[param(decimal, "The tab whose app to keep waiting for")]
        tab: String,
    },

    /// Force quit its app
    ///
    /// End the app of a window that stopped answering. Only that app's
    /// processes go; everything else stays as it was.
    #[effect(own_device)]
    ForceQuitApp {
        /// The tab's id.
        #[param(decimal, "The tab whose app to end")]
        tab: String,
    },
}

/// What the surfaces over the desk can be asked to do: open and show apps,
/// move a window, and the windows of a strip and where they stand, what
/// each shows, the strips and workspaces they are in, the notification
/// centre, the workspace directory and the surfaces above them. Each is
/// one published action on the chrome's resource; the desk publishes the
/// verbs it carries out itself and these are the rest, so no verb has two
/// doors.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.chrome")]
pub enum ChromeEffect {
    /// Open an app
    ///
    /// In a new window of the shown strip, or beside the window named.
    #[effect(own_device, icon = "app-window")]
    OpenApp {
        /// The app's id.
        #[param("The application to open, by its id")]
        app: String,
        /// The tab it opens right after, in that tab's window group.
        #[param(decimal, "The tab to open it beside")]
        beside: Option<String>,
        /// Open it without taking focus.
        #[param("Open it without taking focus")]
        background: Option<bool>,
    },

    /// Show an app
    ///
    /// The shown strip's window of it when there is one, else a new window:
    /// what following a link to the app does.
    #[effect(own_device, idempotent)]
    ShowApp {
        /// The app's id.
        #[param("The application to show, by its id")]
        app: String,
    },

    /// Move a tab elsewhere
    ///
    /// Its window onto another strip of the workspace (or another place in
    /// its own), the tab alone into another workspace as a window of its
    /// own, its window onto another display, or the tab into a window of
    /// its strip.
    #[effect(own_device)]
    MoveTab {
        /// The tab whose window moves.
        #[param(decimal, "The tab to move")]
        tab: String,
        /// Where it goes.
        #[param("Where the tab goes")]
        to: Destination,
    },

    /// Close a window
    ///
    /// Every tab in it, as one close.
    #[effect(own_device)]
    CloseWindow {
        /// The window group's id.
        #[param(decimal, "The window group to close")]
        group: String,
    },

    /// Split right
    ///
    /// Detach the tab into a window of its own, right after the one it is
    /// in.
    #[effect(own_device)]
    SplitRight {
        /// The tab to detach.
        #[param(decimal, "The tab to detach")]
        tab: String,
    },

    /// New group
    ///
    /// Detach the tab into a window of its own at the end of its strip.
    #[effect(own_device)]
    NewGroup {
        /// The tab to detach.
        #[param(decimal, "The tab to detach")]
        tab: String,
    },

    /// Set width
    ///
    /// A preset share of its strip, or an exact share in permille.
    #[effect(own_device, idempotent)]
    SetWidth {
        /// The window group whose width changes.
        #[param(decimal, "The window group")]
        group: String,
        /// The share of the strip, in permille.
        #[param("The share of the strip in permille")]
        permille: Option<u32>,
        /// A preset share.
        #[param("A preset share: half, narrow, normal or wide")]
        preset: Option<WidthPreset>,
    },

    /// Equalize widths
    ///
    /// Every window of `strip` takes the same share of it.
    #[effect(own_device, idempotent)]
    Equalize {
        /// The strip whose windows even out.
        #[param(decimal, "The strip")]
        strip: String,
    },

    /// Maximize a window
    #[effect(own_device, idempotent)]
    Maximize {
        /// The window group.
        #[param(decimal, "The window group")]
        group: String,
        /// Whether it fills its strip.
        #[param("Whether it fills its strip")]
        on: bool,
    },

    /// Fill the display with a window
    #[effect(own_device, idempotent)]
    Fullscreen {
        /// The window group.
        #[param(decimal, "The window group")]
        group: String,
        /// Whether it covers the display.
        #[param("Whether it covers the display")]
        on: bool,
    },

    /// Rename something
    ///
    /// A window, a strip or a workspace, by the one id given; a blank name
    /// gives a window its own title back.
    #[effect(own_device, idempotent)]
    Rename {
        /// The window group to name.
        #[param(decimal, "The window group to rename")]
        group: Option<String>,
        /// The strip to name.
        #[param("The strip to rename, its name or its decimal id")]
        strip: Option<String>,
        /// The workspace to name.
        #[param("The workspace to rename, its name or its decimal id")]
        workspace: Option<String>,
        /// The name it takes.
        #[param("The new name")]
        name: String,
    },

    /// New strip
    ///
    /// An empty strip at the end of the workspace shown.
    #[effect(own_device)]
    NewStrip {
        /// What to call it.
        #[param("The strip's name; a new one by default")]
        name: Option<String>,
    },

    /// Move a strip
    #[effect(own_device)]
    MoveStrip {
        /// The strip to move.
        #[param("The strip to move, its name or its decimal id")]
        strip: String,
        /// The strip it moves in front of; the end when absent.
        #[param("The strip it goes before, its name or its decimal id; the end when absent")]
        before: Option<String>,
    },

    /// Reopen what was closed last
    ///
    /// In the workspace shown, where it was, and focus it.
    #[effect(own_device)]
    ReopenClosed,

    /// Share a window
    ///
    /// With the people of its workspace, or stop sharing it.
    #[effect(own_device)]
    ShareWindow {
        /// The window group.
        #[param(decimal, "The window group")]
        group: String,
        /// Whether it is shared.
        #[param("Whether the window is shared")]
        on: bool,
    },

    /// Take back the desktop control a window holds
    #[effect(own_device)]
    StopDesktopControl {
        /// The window group holding it.
        #[param(decimal, "The window group")]
        group: String,
    },

    /// Step through the strips
    #[effect(own_device)]
    StepStrip {
        /// How many strips on; negative is back.
        #[param("How many strips on; negative goes back")]
        steps: i64,
    },

    /// Switch workspace
    ///
    /// The one named, or that many on (back when negative); the overview's
    /// strip ribbon leaves `strip` shown.
    #[effect(own_device, idempotent)]
    SwitchWorkspace {
        /// The workspace to go to.
        #[param(decimal, "The workspace to go to")]
        workspace: Option<String>,
        /// ...or how many workspaces on.
        #[param("How many workspaces on; negative goes back")]
        delta: Option<i64>,
    },

    /// Show a strip
    ///
    /// The strip of the workspace shown to put in front.
    #[effect(own_device, idempotent)]
    ShowStrip {
        /// The workspace the strip is in.
        #[param(decimal, "The workspace")]
        workspace: String,
        /// The strip to show.
        #[param(decimal, "The strip to show")]
        strip: String,
    },

    /// Show the strips
    ///
    /// The ribbon of the workspace shown, each strip where it stands.
    #[effect(view, own_device, idempotent)]
    ShowStrips,

    /// Pan a strip
    ///
    /// Its horizontal offset, committed when the gesture ends.
    #[effect(own_device)]
    Pan {
        /// The strip shown.
        #[param(decimal, "The strip to pan")]
        strip: String,
        /// Where it goes, in logical px.
        #[param("The offset in logical pixels")]
        offset: f64,
    },

    /// Scroll a window
    #[effect(own_device)]
    Scroll {
        /// The window's primary region, as the snapshot names it.
        #[param("The scroll region the snapshot names")]
        region: String,
        /// What the scroll does.
        #[param("Line, page, an exact offset, the start or the end")]
        action: ScrollAction,
    },

    /// Move through a window's own history
    #[effect(own_device)]
    History {
        /// The tab.
        #[param(decimal, "The tab")]
        tab: String,
        /// `back` or `forward`.
        #[param("The way to go: back or forward")]
        target: String,
    },

    /// Show a workspace's history
    #[effect(own_device)]
    Activity {
        /// The workspace shown.
        #[param(decimal, "The workspace")]
        workspace: String,
        /// Which view: catch up, explore or replay.
        #[param("The view to show")]
        view: ActivityView,
        /// Only what the person named did.
        #[param("Show only what this person did")]
        author: Option<String>,
    },

    /// Close the workspace's history
    #[effect(own_device, idempotent)]
    CloseActivity {
        /// The workspace shown.
        #[param(decimal, "The workspace")]
        workspace: String,
    },

    /// Open or close the notification centre
    #[effect(own_device, idempotent)]
    Notifications {
        /// Whether it is open.
        #[param("Whether the centre is open")]
        open: bool,
    },

    /// New workspace
    ///
    /// An empty one, or one joined from the directory when `workspace` is
    /// given, named when a name is.
    #[effect(own_device)]
    NewWorkspace {
        /// The workspace to open here, by the id it is shared under.
        #[param("The workspace to open here, by the id it is shared under")]
        workspace: Option<String>,
        /// What to call it.
        #[param("What to call the workspace")]
        name: Option<String>,
    },

    /// Show the workspaces this computer may join
    #[effect(view, own_device)]
    JoinWorkspace,

    /// Leave a workspace
    ///
    /// Here on this computer: the one named, or the one shown.
    #[effect(own_device)]
    LeaveWorkspace {
        /// The workspace to leave.
        #[param("The workspace to leave, its name or its decimal id")]
        workspace: Option<String>,
    },

    /// Delete a workspace
    ///
    /// The workspace shown, when it is the person's own and this computer is
    /// the only one holding it: it goes, with everything in it. Deleting is
    /// asked first, so nothing goes until the person says `confirmed`.
    #[effect(own_device)]
    DeleteWorkspace {
        /// The workspace to delete.
        #[param("The workspace to delete, its name or its decimal id")]
        workspace: Option<String>,
        /// The person answered the question that deleting asks: yes.
        #[param("The person said yes to the question deleting asks")]
        confirmed: Option<bool>,
    },

    /// Open the workspace's settings
    #[effect(view, own_device)]
    WorkspaceSettings,

    /// Run a desktop shortcut
    ///
    /// By its id in the shortcut table, as its chord does.
    #[effect(own_device)]
    Shortcut {
        /// The shortcut's id.
        #[param("The shortcut's id in the shortcut table")]
        action: String,
    },

    /// Open or close who is here
    #[effect(view, own_device, idempotent)]
    ToggleCollaborators,

    /// Show a person
    #[effect(view, own_device)]
    ShowPerson {
        /// The person, as the roster names them.
        #[param("The person to show")]
        person: String,
    },

    /// Open or close the overview
    ///
    /// Whether it is to be open; toggled when absent.
    #[effect(view, own_device, idempotent)]
    Overview {
        /// Whether it is open.
        #[param("Whether the overview is open; toggled when absent")]
        open: Option<bool>,
    },

    /// Show the top bar over a full screen window
    ///
    /// As the Super key and a finger swiped down from the top edge do.
    #[effect(view, own_device, idempotent)]
    RevealTopBar,

    /// Open or close quick settings
    #[effect(view, own_device, idempotent)]
    QuickSettings,

    /// Open or close the Tilo menu
    #[effect(view, own_device, idempotent)]
    TiloMenu {
        /// Whether it is open.
        #[param("Whether the menu is open; toggled when absent")]
        open: Option<bool>,
    },

    /// Open or close the workspace menu
    #[effect(view, own_device, idempotent)]
    WorkspaceMenu,

    /// Run a fixed desktop shortcut
    ///
    /// By its id in the shortcut catalogue: a chord Settings cannot rebind,
    /// such as closing the window in front or opening the capture studio.
    #[effect(own_device)]
    FixedShortcut {
        /// The fixed shortcut's id.
        #[param("The fixed shortcut's id in the shortcut catalogue")]
        action: String,
    },

    /// Lift a window out of its strip, or put it back
    #[effect(own_device, idempotent)]
    Float {
        /// The window group.
        #[param(decimal, "The window group")]
        group: String,
        /// Whether it floats above the desk.
        #[param("Whether it floats above the desk")]
        on: bool,
    },
}

/// A verb a host page or a surface dispatches, from either resource: the
/// desktop's own verbs and the ones its surfaces over it carry out, named by
/// the one vocabulary's ids.
#[derive(Clone, Debug, PartialEq)]
pub enum Verb {
    Desk(DeskEffect),
    Chrome(ChromeEffect),
}

impl Verb {
    /// The verb `effect` with `arguments`, as a caller dispatched it.
    ///
    /// # Errors
    /// Returns why no verb of the desk's is named by the id and arguments.
    pub fn decode(
        effect: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Self, String> {
        if let Ok(verb) = DeskEffect::decode(effect, arguments) {
            return Ok(Self::Desk(verb));
        }
        ChromeEffect::decode(effect, arguments)
            .map(Self::Chrome)
            .map_err(|error| error.to_string())
    }

    /// This verb's id among the declared ones.
    #[must_use]
    pub fn id(&self) -> &'static str {
        match self {
            Self::Desk(verb) => verb.id(),
            Self::Chrome(verb) => verb.id(),
        }
    }

    /// The verb `payload` encodes, from either vocabulary: an invocation's
    /// effect id and the arguments its caller gave.
    ///
    /// # Errors
    /// Returns why no verb is named by it.
    pub fn from_payload(payload: &[u8]) -> Result<Self, String> {
        #[derive(serde::Deserialize)]
        struct Payload {
            effect: String,
            #[serde(default)]
            args: serde_json::Map<String, serde_json::Value>,
        }
        let Payload { effect, args } =
            serde_json::from_slice(payload).map_err(|error| format!("Unknown verb: {error}"))?;
        Self::decode(&effect, &args)
    }

    /// The verb as the wire spells it: its id and its arguments.
    ///
    /// # Errors
    /// Never: every verb of the vocabulary is its declaration's arguments.
    pub fn arguments(&self) -> serde_json::Map<String, serde_json::Value> {
        match self {
            Self::Desk(verb) => Effect::arguments(verb),
            Self::Chrome(verb) => Effect::arguments(verb),
        }
    }
}

impl From<DeskEffect> for Verb {
    fn from(verb: DeskEffect) -> Self {
        Self::Desk(verb)
    }
}

impl From<ChromeEffect> for Verb {
    fn from(verb: ChromeEffect) -> Self {
        Self::Chrome(verb)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub enum ScrollAction {
    Line(i8),
    Page(i8),
    Absolute(f64),
    Start,
    End,
}

impl From<ScrollAction> for ui::widget::PrimaryScrollAction {
    fn from(value: ScrollAction) -> Self {
        match value {
            ScrollAction::Line(v) => Self::Line(v),
            ScrollAction::Page(v) => Self::Page(v),
            ScrollAction::Absolute(v) => Self::Absolute(v),
            ScrollAction::Start => Self::Start,
            ScrollAction::End => Self::End,
        }
    }
}

/// A navigation command checked against the tree it will change.
#[derive(Clone, Debug, PartialEq)]
pub enum Admitted {
    Select(RouteIds),
    Close(RouteIds),
    /// Reopen the newest close of the active workspace.
    Reopen(WorkspaceSlot),
    Width(RouteIds, u32),
    Maximize(RouteIds, bool),
    /// Cover the display with the window, or give it back.
    Fullscreen(RouteIds, bool),
    /// A person's name for the window; empty clears it.
    Rename(RouteIds, String),
    Strip(WorkspaceSlot, StripId),
    Workspace(WorkspaceSlot),
    /// An application id; the host checked that it can open it.
    Launch(String),
    /// Show application `app`: the shown strip's window of it when there is
    /// one by the time the command applies, else a new one. Decided then,
    /// not at dispatch, since a workspace's desktop can arrive in between: a
    /// page opening its workspace's app must find the workspace's window, not add
    /// one of its own for everyone.
    Open(String),
    /// Open desktop search, with this query when one was given.
    Search(Option<String>),
    /// Pan the shown strip to a finite offset in logical px.
    Pan(StripId, f64),
    /// Scroll the primary region of the window's active tab.
    Scroll(RouteIds, ScrollAction),
    /// Move the tab's own navigation history.
    History(RouteIds, HistoryDirection),
    /// Open the workspace's history at a view, showing only what the
    /// named person did when one is named.
    Activity(WorkspaceSlot, ActivityView, Option<String>),
    CloseActivity(WorkspaceSlot),
    /// Open or close the notification centre.
    NotificationCenter(bool),
    /// Open `app` as a tab right after the tab at the route, in its group,
    /// taking focus unless the last field says not to.
    TabOpenBeside(RouteIds, String, bool),
    /// Carry the window into another strip of its workspace, before the
    /// window named when one is: a place in its own strip reorders it.
    MoveToStrip(RouteIds, StripId, Option<GroupId>),
    /// Put the tab into the window `GroupId` of its own strip.
    JoinGroup(RouteIds, GroupId),
    /// Take the tab into another workspace, as a window of its own.
    MoveToWorkspace(RouteIds, WorkspaceSlot),
    /// Show the window on another display.
    MoveToDisplay(RouteIds, u32),
    /// Give the window a preset share of its strip.
    WidthPreset(RouteIds, tilo_desktop_model::WindowWidth),
    /// Give every window of the strip the same share of it.
    EqualizeWidths(RouteIds),
    /// Share the window with the people of its workspace, or stop.
    ShareWindow(RouteIds, bool),
    /// Detach the tab into a window of its own, right after the one it is
    /// in.
    SplitRight(RouteIds),
    /// Detach the tab into a window of its own at the end of its strip.
    MoveToNewGroup(RouteIds),
    /// Close every tab of the window, as one close.
    CloseWindow(RouteIds),
    /// Take back the app's control of the desktop.
    StopDesktopControl(RouteIds),
    /// Lift the window above the desk, or put it back into its strip.
    Float(RouteIds, bool),
}

pub fn scroll_region(route: RouteIds, key: &str) -> String {
    format!(
        "{}/{}/{}/{}/{key}",
        route.workspace.0, route.strip.0, route.group, route.tab
    )
}

fn parse_scroll_region(region: &str) -> Result<RouteIds, String> {
    let mut parts = region.splitn(5, '/');
    let mut next = || parts.next().ok_or("Scroll target is no longer active");
    let route = Route {
        workspace: next()?.into(),
        strip: next()?.into(),
        group: next()?.into(),
        tab: next()?.into(),
    };
    next()?;
    route.ids()
}

/// Which commands this surface performs without host services. The rest are
/// refused with a reason rather than silently ignored. A host that installs
/// a history source adds Catch up, Explore and Replay, and one that installs
/// notifications adds the centre.
pub const CAPABILITIES: Capabilities = Capabilities {
    select: true,
    width: true,
    maximize: true,
    strips: true,
    commands: true,
    global_search: true,
    workspace_select: true,
    workspace_history: false,
    workspace_replay: false,
    close: true,
    close_inactive: false,
    launch: true,
    pan: true,
    scroll: true,
    history: true,
    swipe: true,
    notifications: false,
};

/// The longest name a person may give a window.
pub const MAX_NAME_CHARS: usize = 200;

/// The window group `group` names, wherever in the tree it stands, and the
/// tab focus lands on in it.
///
/// # Errors
/// Refuses an id that is not decimal text, or a group the tree does not
/// hold with a tab to focus.
pub fn group_route(tree: &DesktopTree, group: &str) -> Result<RouteIds, String> {
    let group = id(group)?;
    for workspace in &tree.workspaces {
        for strip in &workspace.strips {
            for held in &strip.window_groups {
                if held.id == group && held.tabs.iter().any(|tab| tab.id == held.active_tab) {
                    return Ok(RouteIds {
                        workspace: workspace.slot,
                        strip: strip.id,
                        group: held.id,
                        tab: held.active_tab,
                    });
                }
            }
        }
    }
    Err("Window unavailable".into())
}

/// Whether the window at `ids` holds a tab besides `ids.tab`. A window of one
/// tab is that tab's window already: there is nothing to split, close or group
/// beside it, and a split asked of it is answered rather than done.
#[must_use]
pub fn has_a_second_tab(tree: &DesktopTree, ids: &RouteIds) -> bool {
    tree.workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .find(|group| group.id == ids.group)
        .is_some_and(|group| group.tabs.len() >= 2)
}

/// The window the tab `tab` is in, wherever in the tree it stands.
///
/// # Errors
/// Refuses an id that is not decimal text, or a tab the tree does not hold.
pub fn tab_route(tree: &DesktopTree, tab: &str) -> Result<RouteIds, String> {
    let tab = id(tab)?;
    for workspace in &tree.workspaces {
        for strip in &workspace.strips {
            for group in &strip.window_groups {
                if group.tabs.iter().any(|held| held.id == tab) {
                    return Ok(RouteIds {
                        workspace: workspace.slot,
                        strip: strip.id,
                        group: group.id,
                        tab,
                    });
                }
            }
        }
    }
    Err("Window unavailable".into())
}

/// A person's name for the window at `ids`: trimmed, within the length a
/// name may be, and refused for a window the tree no longer holds.
fn named(ids: RouteIds, name: &str, tree: &DesktopTree) -> Result<Admitted, String> {
    if name.chars().count() > MAX_NAME_CHARS {
        return Err("Window name too long".into());
    }
    locate(tree, ids)
        .map(|_| Admitted::Rename(ids, name.trim().to_owned()))
        .ok_or_else(|| "Window unavailable".into())
}

/// Check `verb` against `tree` without changing either.
///
/// # Errors
/// Refuses a malformed or absent identity, one the tree does not hold, or a
/// verb this surface does not carry out.
pub fn admit(verb: &Verb, tree: &DesktopTree) -> Result<Admitted, String> {
    admit_with(verb, tree, &CAPABILITIES)
}

/// History at `view` over `workspace`, at `author`'s doings when named,
/// if this surface can show that view.
fn admit_activity(
    workspace: WorkspaceSlot,
    view: ActivityView,
    author: Option<String>,
    capabilities: &Capabilities,
) -> Result<Admitted, String> {
    match view.unavailable(capabilities) {
        Some(reason) => Err(reason.into()),
        None => Ok(Admitted::Activity(workspace, view, author)),
    }
}

/// The workspace whose closes a `reopen` undoes: the one the surface shows.
///
/// The closes a workspace remembers belong to the device the person closed
/// them on, so what can be reopened is read when the command applies; here
/// the shown workspace is all there is to check.
fn admit_reopen(tree: &DesktopTree) -> Result<Admitted, String> {
    tree.active_workspace()
        .map(|workspace| Admitted::Reopen(workspace.slot))
        .ok_or_else(|| "Workspace unavailable".into())
}

/// [`admit`] for a surface performing `capabilities`.
///
/// # Errors
/// As [`admit`].
pub fn admit_with(
    verb: &Verb,
    tree: &DesktopTree,
    capabilities: &Capabilities,
) -> Result<Admitted, String> {
    match verb {
        Verb::Desk(verb) => admit_desk(verb, tree),
        Verb::Chrome(verb) => admit_chrome(verb, tree, capabilities),
    }
}

/// The desk's own verbs, as the surface carries them out: the ones a page
/// dispatches to move around the windows it shows.
fn admit_desk(verb: &DeskEffect, tree: &DesktopTree) -> Result<Admitted, String> {
    match verb {
        DeskEffect::CloseTab { tab } => tab_route(tree, tab).map(Admitted::Close),
        DeskEffect::Focus { tab } => tab_route(tree, tab).map(Admitted::Select),
        DeskEffect::Search { query } => match query {
            None => Ok(Admitted::Search(None)),
            Some(query) if ui::search::system::query_input_is_bounded(query) => {
                Ok(Admitted::Search(Some(query.clone())))
            }
            Some(_) => Err("Search query too long".into()),
        },
        DeskEffect::StepFocus { .. }
        | DeskEffect::FocusDisplay { .. }
        | DeskEffect::MoveToDisplay { .. }
        | DeskEffect::ChooseSearchRow { .. }
        | DeskEffect::Launchpad { .. }
        | DeskEffect::WaitWindow { .. }
        | DeskEffect::ForceQuitApp { .. } => Err("The desk carries this out itself".into()),
    }
}

/// The verbs the surfaces over the desk carry out, as the surface changes
/// what it shows.
#[expect(
    clippy::too_many_lines,
    reason = "one verb, one arm: every verb the surface carries out is admitted here"
)]
fn admit_chrome(
    verb: &ChromeEffect,
    tree: &DesktopTree,
    capabilities: &Capabilities,
) -> Result<Admitted, String> {
    let workspace = |value: &str| -> Result<WorkspaceSlot, String> {
        let workspace = WorkspaceSlot(id(value)?);
        tree.workspaces
            .iter()
            .any(|w| w.slot == workspace)
            .then_some(workspace)
            .ok_or_else(|| "Workspace unavailable".into())
    };
    match verb {
        ChromeEffect::OpenApp {
            app,
            beside,
            background,
        } => match beside {
            Some(beside) => tab_route(tree, beside)
                .and_then(|ids| admit_beside(ids, app, background.unwrap_or(false))),
            None => admit_launch(tree, app),
        },
        ChromeEffect::ShowApp { app } => admit_open(tree, app),
        ChromeEffect::MoveTab { tab, to } => {
            let ids = tab_route(tree, tab)?;
            admit_move(ids, to, tree)
        }
        ChromeEffect::CloseWindow { group } => group_route(tree, group).map(Admitted::CloseWindow),
        ChromeEffect::SplitRight { tab } => {
            let ids = tab_route(tree, tab)?;
            if !has_a_second_tab(tree, &ids) {
                return Err(t("window-menu-split-one-tab"));
            }
            Ok(Admitted::SplitRight(ids))
        }
        ChromeEffect::NewGroup { tab } => tab_route(tree, tab).map(Admitted::MoveToNewGroup),
        ChromeEffect::SetWidth {
            group,
            permille,
            preset,
        } => {
            let ids = group_route(tree, group)?;
            match (preset, permille) {
                (Some(preset), _) => Ok(Admitted::WidthPreset(ids, (*preset).into())),
                (None, permille) => {
                    let permille = permille.unwrap_or(500).clamp(
                        ui::widget::WIDTH_MIN_PERMILLE,
                        ui::widget::WIDTH_MAX_PERMILLE,
                    );
                    Ok(Admitted::Width(ids, permille))
                }
            }
        }
        ChromeEffect::Equalize { strip } => {
            let strip = StripId(id(strip)?);
            let workspace = tree
                .workspaces
                .iter()
                .find(|workspace| {
                    active_strip(tree, workspace) == Some(strip)
                        && workspace.strips.iter().any(|held| held.id == strip)
                })
                .ok_or_else(|| "Strip is no longer active".to_owned())?;
            let group = workspace
                .strips
                .iter()
                .find(|held| held.id == strip)
                .and_then(|held| {
                    held.window_groups
                        .iter()
                        .find(|group| group.tabs.iter().any(|tab| tab.id == group.active_tab))
                })
                .ok_or_else(|| "Window unavailable".to_owned())?;
            Ok(Admitted::EqualizeWidths(RouteIds {
                workspace: workspace.slot,
                strip,
                group: group.id,
                tab: group.active_tab,
            }))
        }
        ChromeEffect::Maximize { group, on } => {
            group_route(tree, group).map(|ids| Admitted::Maximize(ids, *on))
        }
        ChromeEffect::Fullscreen { group, on } => {
            group_route(tree, group).map(|ids| Admitted::Fullscreen(ids, *on))
        }
        ChromeEffect::Float { group, on } => {
            group_route(tree, group).map(|ids| Admitted::Float(ids, *on))
        }
        ChromeEffect::Rename { group, name, .. } => match group {
            Some(group) => named(group_route(tree, group)?, name, tree),
            None => Err("Name a window, a strip or a workspace to rename".into()),
        },
        ChromeEffect::ReopenClosed => admit_reopen(tree),
        ChromeEffect::ShareWindow { group, on } => {
            group_route(tree, group).map(|ids| Admitted::ShareWindow(ids, *on))
        }
        ChromeEffect::StopDesktopControl { group } => {
            group_route(tree, group).map(Admitted::StopDesktopControl)
        }
        ChromeEffect::Pan { strip, offset } => admit_pan(tree, strip, *offset),
        ChromeEffect::Scroll { region, action } => {
            let ids = parse_scroll_region(region)?;
            if active_route(tree) != Some(ids) {
                return Err("Scroll target is no longer active".into());
            }
            Ok(Admitted::Scroll(ids, *action))
        }
        ChromeEffect::History { tab, target } => {
            let direction = history_direction(target)?;
            tab_route(tree, tab).map(|ids| Admitted::History(ids, direction))
        }
        ChromeEffect::Activity {
            workspace: w,
            view,
            author,
        } => admit_activity(
            active_workspace(tree, w)?,
            *view,
            author.clone(),
            capabilities,
        ),
        ChromeEffect::CloseActivity { workspace: w } => {
            active_workspace(tree, w).map(Admitted::CloseActivity)
        }
        ChromeEffect::Notifications { open } => admit_notifications(*open, capabilities),
        ChromeEffect::SwitchWorkspace {
            workspace: Some(w), ..
        } => workspace(w).map(Admitted::Workspace),
        ChromeEffect::ShowStrip {
            workspace: w,
            strip,
        } => {
            let workspace = active_workspace(tree, w)?;
            let strip = StripId(id(strip)?);
            tree.workspaces
                .iter()
                .find(|held| held.slot == workspace)
                .is_some_and(|held| held.strips.iter().any(|known| known.id == strip))
                .then_some(Admitted::Strip(workspace, strip))
                .ok_or_else(|| "Strip unavailable".into())
        }
        ChromeEffect::NewStrip { .. }
        | ChromeEffect::MoveStrip { .. }
        | ChromeEffect::StepStrip { .. }
        | ChromeEffect::NewWorkspace { .. }
        | ChromeEffect::JoinWorkspace
        | ChromeEffect::LeaveWorkspace { .. }
        | ChromeEffect::DeleteWorkspace { .. }
        | ChromeEffect::WorkspaceSettings
        | ChromeEffect::Shortcut { .. }
        | ChromeEffect::FixedShortcut { .. }
        | ChromeEffect::ToggleCollaborators
        | ChromeEffect::ShowStrips
        | ChromeEffect::ShowPerson { .. }
        | ChromeEffect::Overview { .. }
        | ChromeEffect::RevealTopBar
        | ChromeEffect::QuickSettings
        | ChromeEffect::TiloMenu { .. }
        | ChromeEffect::WorkspaceMenu
        | ChromeEffect::SwitchWorkspace {
            workspace: None, ..
        } => Err("The surfaces carry this out themselves".into()),
    }
}

/// A launch of `app`, checked against the desktop it opens on.
fn admit_launch(tree: &DesktopTree, app: &str) -> Result<Admitted, String> {
    // A workspace may hold no strip yet: the first window opened in it makes
    // the strip it goes in.
    tree.active_workspace().ok_or("Workspace unavailable")?;
    if app.is_empty() {
        return Err("Application unavailable".into());
    }
    Ok(Admitted::Launch(app.to_owned()))
}

/// A tab opening beside the window at `ids`, with the app it opens.
fn admit_beside(ids: RouteIds, app: &str, background: bool) -> Result<Admitted, String> {
    if app.is_empty() {
        return Err("Application unavailable".into());
    }
    Ok(Admitted::TabOpenBeside(ids, app.to_owned(), background))
}

/// Where a window goes: the destination one of `to`'s ids names, checked
/// against the tree.
fn admit_move(ids: RouteIds, to: &Destination, tree: &DesktopTree) -> Result<Admitted, String> {
    if let Some(group) = &to.group {
        let target: GroupId = id(group)?;
        let there = tree
            .workspaces
            .iter()
            .find(|workspace| workspace.slot == ids.workspace)
            .and_then(|workspace| workspace.strips.iter().find(|held| held.id == ids.strip))
            .is_some_and(|strip| {
                target != ids.group && strip.window_groups.iter().any(|group| group.id == target)
            });
        return there
            .then_some(Admitted::JoinGroup(ids, target))
            .ok_or_else(|| "Window unavailable".into());
    }
    if let Some(strip) = &to.strip {
        let target = StripId(id(strip)?);
        let before = to.before.as_deref().map(id).transpose()?;
        let strip_held = tree
            .workspaces
            .iter()
            .find(|workspace| workspace.slot == ids.workspace)
            .and_then(|workspace| workspace.strips.iter().find(|held| held.id == target))
            .is_some_and(|strip| {
                before.is_none_or(|before| {
                    before != ids.group
                        && strip.window_groups.iter().any(|group| group.id == before)
                })
            });
        // A place in the strip the window already stands on reorders it —
        // the strip's end when no window is named before it; any other
        // strip of its workspace carries it there.
        let there = strip_held;
        return there
            .then_some(Admitted::MoveToStrip(ids, target, before))
            .ok_or_else(|| "Strip unavailable".into());
    }
    if let Some(workspace) = &to.workspace {
        let workspace = WorkspaceSlot(id(workspace)?);
        let there = tree.workspaces.iter().any(|held| held.slot == workspace);
        return (there && workspace != ids.workspace)
            .then_some(Admitted::MoveToWorkspace(ids, workspace))
            .ok_or_else(|| "Workspace unavailable".into());
    }
    if let Some(display) = &to.display {
        let display: u32 = display
            .parse()
            .map_err(|_| "Display unavailable".to_owned())?;
        let connected = tree.outputs.displays.iter().any(|held| held.id == display)
            && tree.outputs.display_of(ids.group) != Some(display);
        return connected
            .then_some(Admitted::MoveToDisplay(ids, display))
            .ok_or_else(|| "Display unavailable".into());
    }
    Err("The surface carries a tab to a strip, workspace, display or window only".into())
}

fn admit_open(tree: &DesktopTree, app: &str) -> Result<Admitted, String> {
    let workspace = tree.active_workspace().ok_or("Workspace unavailable")?;
    let strip = active_strip(tree, workspace).ok_or("Strip unavailable")?;
    if app.is_empty() {
        return Err("Application unavailable".into());
    }
    // A window already showing the app is the one the link means; a second
    // copy would split the person's place in it.
    let shown = workspace
        .strips
        .iter()
        .find(|s| s.id == strip)
        .into_iter()
        .flat_map(|s| s.window_groups.iter())
        .find_map(|group| {
            let tab = group.tabs.iter().find(|tab| {
                tilo_desktop_model::apps::canonical_app_id(&tab.tile).as_deref() == Some(app)
            })?;
            Some(RouteIds {
                workspace: workspace.slot,
                strip,
                group: group.id,
                tab: tab.id,
            })
        });
    Ok(shown.map_or_else(|| Admitted::Open(app.to_owned()), Admitted::Select))
}

/// What [`Admitted::Open`] of `app` does on `tree` now: select the shown
/// strip's window of it, or launch one.
pub(crate) fn resolve_open(tree: &DesktopTree, app: &str) -> Admitted {
    match admit_open(tree, app) {
        Ok(Admitted::Select(ids)) => Admitted::Select(ids),
        _ => Admitted::Launch(app.to_owned()),
    }
}

/// A window of `app` in the active workspace, the shown strip's first:
/// where choosing the app should take a person who already has it open,
/// rather than to a second copy that splits their place in it.
pub fn window_of_app(tree: &DesktopTree, app: &str) -> Option<RouteIds> {
    let workspace = tree.active_workspace()?;
    let shown = active_strip(tree, workspace);
    let strips = workspace
        .strips
        .iter()
        .filter(|strip| Some(strip.id) == shown)
        .chain(
            workspace
                .strips
                .iter()
                .filter(|strip| Some(strip.id) != shown),
        );
    strips
        .flat_map(|strip| {
            strip
                .window_groups
                .iter()
                .map(move |group| (strip.id, group))
        })
        .find_map(|(strip, group)| {
            let tab = group.tabs.iter().find(|tab| {
                tilo_desktop_model::apps::canonical_app_id(&tab.tile).as_deref() == Some(app)
            })?;
            Some(RouteIds {
                workspace: workspace.slot,
                strip,
                group: group.id,
                tab: tab.id,
            })
        })
}

fn admit_notifications(open: bool, capabilities: &Capabilities) -> Result<Admitted, String> {
    capabilities
        .notifications
        .then_some(Admitted::NotificationCenter(open))
        .ok_or_else(|| "Notifications are unavailable on this desktop".into())
}

fn active_workspace(tree: &DesktopTree, value: &str) -> Result<WorkspaceSlot, String> {
    let workspace = WorkspaceSlot(id(value)?);
    if !tree.workspaces.iter().any(|w| w.slot == workspace) {
        return Err("Workspace unavailable".into());
    }
    (tree.active == workspace)
        .then_some(workspace)
        .ok_or_else(|| "Workspace is no longer active".into())
}

fn admit_pan(tree: &DesktopTree, strip: &str, offset: f64) -> Result<Admitted, String> {
    if !offset.is_finite() {
        return Err("Invalid pan offset".into());
    }
    let strip = StripId(id(strip)?);
    let shown = tree
        .active_workspace()
        .and_then(|workspace| active_strip(tree, workspace));
    if shown != Some(strip) {
        return Err("Strip is no longer active".into());
    }
    Ok(Admitted::Pan(strip, offset))
}

/// The group `ids` names, if the tab is still in it.
pub fn locate(tree: &DesktopTree, ids: RouteIds) -> Option<&WindowGroup> {
    tree.workspaces
        .iter()
        .find(|w| w.slot == ids.workspace)?
        .strips
        .iter()
        .find(|s| s.id == ids.strip)?
        .window_groups
        .iter()
        .find(|g| g.id == ids.group)
        .filter(|g| g.tabs.iter().any(|t| t.id == ids.tab))
}

/// The strip a workspace shows: its recorded active strip, else its first.
pub fn active_strip(tree: &DesktopTree, workspace: &Workspace) -> Option<StripId> {
    tree.active_strip
        .get(&workspace.slot)
        .copied()
        .filter(|id| workspace.strips.iter().any(|s| s.id == *id))
        .or_else(|| workspace.strips.first().map(|s| s.id))
}

/// The focused window of the active workspace.
pub fn active_route(tree: &DesktopTree) -> Option<RouteIds> {
    let workspace = tree.active_workspace()?;
    let strip = active_strip(tree, workspace)?;
    let group_id = tree.resolve_active_group(strip, |_| false)?;
    let group = workspace
        .strips
        .iter()
        .find(|s| s.id == strip)?
        .window_groups
        .iter()
        .find(|g| g.id == group_id)?;
    group
        .tabs
        .iter()
        .any(|t| t.id == group.active_tab)
        .then_some(RouteIds {
            workspace: workspace.slot,
            strip,
            group: group.id,
            tab: group.active_tab,
        })
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[allow(
    clippy::struct_excessive_bools,
    reason = "mirrors the wire contract's independent capability flags"
)]
pub struct Capabilities {
    pub select: bool,
    pub width: bool,
    pub maximize: bool,
    pub strips: bool,
    pub commands: bool,
    pub global_search: bool,
    pub workspace_select: bool,
    pub workspace_history: bool,
    pub workspace_replay: bool,
    pub close: bool,
    pub close_inactive: bool,
    pub launch: bool,
    pub pan: bool,
    pub scroll: bool,
    /// Per-window back and forward through the tab's own history.
    pub history: bool,
    /// The surface takes sideways wheel input itself: a trackpad swipe pans
    /// the strip, a mouse wheel's spin alike, and focus follows. A
    /// host must leave it the wheel rather than turn it into `pan` commands.
    pub swipe: bool,
    /// The surface shows notification banners and opens the notification
    /// centre on `notification-center`.
    pub notifications: bool,
}

/// The shown strip's horizontal geometry, in logical px.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct DesktopPan {
    pub workspace: String,
    pub strip: String,
    pub offset: f64,
    pub viewport: f64,
    pub content: f64,
    pub max: f64,
}

/// The focused window's primary vertical scroll region, in logical px.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ScrollState {
    pub region: String,
    pub offset: f64,
    pub viewport: f64,
    pub content: f64,
    pub max: f64,
}

/// Whether the focused window's tab can move through its own history.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HistoryState {
    pub can_back: bool,
    pub can_forward: bool,
}

/// Presentation the surface holds beside the tree: nothing here is saved
/// or shared with peers.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Presentation {
    pub pan: Option<DesktopPan>,
    pub scroll: Option<ScrollState>,
    pub history: Option<HistoryState>,
    /// The host installed a history source: Catch up, Explore and Replay.
    pub retained_history: bool,
    /// The host installed notifications, and what they show.
    pub notifications: Option<NotificationsState>,
}

/// What the surface's notifications show, for a host's tray control.
#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NotificationsState {
    /// Banners on screen.
    pub active: usize,
    pub retained: usize,
    pub center_open: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub route: Route,
    /// The app the window shows, as links name it; `None` for a window that
    /// shows no app of its own.
    pub app: Option<String>,
    pub label: String,
    pub strip_label: String,
    pub workspace_label: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct StripEntry {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct WorkspaceEntry {
    pub id: String,
    pub label: String,
    pub workspace: String,
    pub owner: Option<String>,
    pub strips: Vec<StripEntry>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Application {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub version: u8,
    pub generation: String,
    pub entries: Vec<Entry>,
    pub workspaces: Vec<WorkspaceEntry>,
    pub applications: Vec<Application>,
    pub active_workspace: String,
    pub active_strip: Option<String>,
    pub active: Option<Route>,
    pub width_permille: Option<u32>,
    pub maximized: bool,
    pub capabilities: Capabilities,
    pub desktop_pan: Option<DesktopPan>,
    pub history: Option<HistoryState>,
    pub scroll: Option<ScrollState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notifications: Option<NotificationsState>,
}

pub fn snapshot(tree: &DesktopTree, generation: &str, applications: Vec<Application>) -> Snapshot {
    snapshot_with(tree, generation, applications, &Presentation::default())
}

/// [`snapshot`] with what the surface presents beside the tree. Stale
/// presentation (another strip, workspace or window) is left out.
pub fn snapshot_with(
    tree: &DesktopTree,
    generation: &str,
    applications: Vec<Application>,
    presentation: &Presentation,
) -> Snapshot {
    let mut entries = Vec::new();
    for workspace in &tree.workspaces {
        for strip in &workspace.strips {
            for group in &strip.window_groups {
                for tab in &group.tabs {
                    entries.push(Entry {
                        route: Route::from_ids(RouteIds {
                            workspace: workspace.slot,
                            strip: strip.id,
                            group: group.id,
                            tab: tab.id,
                        }),
                        app: tilo_desktop_model::apps::canonical_app_id(&tab.tile),
                        label: tab.title.clone(),
                        strip_label: strip.name.clone(),
                        workspace_label: workspace.name.clone(),
                    });
                }
            }
        }
    }
    let active = active_route(tree);
    let active_group = active.and_then(|ids| locate(tree, ids));
    let shown_strip = tree
        .active_workspace()
        .and_then(|w| active_strip(tree, w))
        .map(|s| s.0.to_string());
    let desktop_pan = presentation.pan.clone().filter(|pan| {
        pan.workspace == tree.active.0.to_string() && Some(&pan.strip) == shown_strip.as_ref()
    });
    let scroll = presentation.scroll.clone().filter(|scroll| {
        active.is_some_and(|ids| parse_scroll_region(&scroll.region).ok() == Some(ids))
    });
    let history = presentation.history.filter(|_| active.is_some());
    Snapshot {
        version: 1,
        generation: generation.to_owned(),
        entries,
        workspaces: tree
            .workspaces
            .iter()
            .map(|w| WorkspaceEntry {
                id: w.slot.0.to_string(),
                label: w.name.clone(),
                workspace: w.id().to_string(),
                owner: w.owner().cloned(),
                strips: w
                    .strips
                    .iter()
                    .map(|s| StripEntry {
                        id: s.id.0.to_string(),
                        label: s.name.clone(),
                    })
                    .collect(),
            })
            .collect(),
        applications,
        active_workspace: tree.active.0.to_string(),
        active_strip: shown_strip,
        active: active.map(Route::from_ids),
        width_permille: active_group.map(|g| g.width.permille()),
        maximized: active_group.is_some_and(|g| g.maximized),
        capabilities: presentation.capabilities(),
        desktop_pan,
        history,
        scroll,
        notifications: presentation.notifications,
    }
}

impl Presentation {
    /// [`CAPABILITIES`] plus what the host's installed services add.
    pub fn capabilities(&self) -> Capabilities {
        Capabilities {
            workspace_history: self.retained_history,
            workspace_replay: self.retained_history,
            notifications: self.notifications.is_some(),
            ..CAPABILITIES
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_desktop_model::{Strip, Tab, TileContent, WindowWidth};

    fn tree() -> DesktopTree {
        let mut workspace = Workspace::new(WorkspaceSlot(u64::MAX), "Desk");
        let mut strip = Strip::new(StripId(9_007_199_254_740_993), "Main");
        strip.window_groups = vec![WindowGroup {
            id: 3,
            tabs: vec![Tab {
                id: 4,
                title: "Notes".into(),
                tile: TileContent::LocalApp {
                    app_id: "so.tilo.notes".into(),
                    label: "Notes".into(),
                    note: String::new(),
                },
                app_title: None,
                view: String::new(),
                pinned: false,
            }],
            active_tab: 4,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }];
        workspace.strips = vec![strip];
        DesktopTree {
            active: workspace.slot,
            workspaces: vec![workspace],
            ..DesktopTree::default()
        }
    }

    /// A verb as a page sends it: its resource's effect id and the
    /// arguments the caller gives.
    fn decode<E: Effect>(effect: &str, args: serde_json::Value) -> Result<E, String> {
        let payload = serde_json::Value::Object(serde_json::Map::from_iter([
            ("effect".to_owned(), serde_json::Value::from(effect)),
            ("args".to_owned(), args),
        ]));
        let payload = serde_json::to_vec(&payload).unwrap();
        E::from_payload(&payload).map_err(|error| error.to_string())
    }

    fn desk(effect: &str, args: serde_json::Value) -> Verb {
        Verb::Desk(decode(effect, args).unwrap())
    }

    fn chrome(effect: &str, args: serde_json::Value) -> Verb {
        Verb::Chrome(decode(effect, args).unwrap())
    }

    fn admitted(tree: &DesktopTree, verb: &Verb) -> Result<Admitted, String> {
        admit(verb, tree)
    }

    /// Every verb of either vocabulary decodes from its declaration's id, and
    /// is asked for with exactly the arguments its declaration makes
    /// optional: nothing of it can be dropped, or invented, on the wire.
    #[test]
    fn every_verb_decodes_from_its_id_and_refuses_a_bad_argument() {
        for declaration in DeskEffect::declarations()
            .into_iter()
            .chain(ChromeEffect::declarations())
        {
            let none_needed = declaration.params.iter().all(|param| !param.required);
            let decoded = DeskEffect::decode(declaration.id, &serde_json::Map::default()).is_ok()
                || ChromeEffect::decode(declaration.id, &serde_json::Map::default()).is_ok();
            assert_eq!(
                decoded, none_needed,
                "{}: arguments are required only where its declaration asks",
                declaration.id
            );
        }
        assert!(decode::<DeskEffect>("no-such-verb", serde_json::json!({})).is_err());
        assert_eq!(
            decode::<DeskEffect>("focus", serde_json::json!({"tab":"4"})).ok(),
            Some(DeskEffect::Focus { tab: "4".into() })
        );
        assert!(decode::<DeskEffect>("focus", serde_json::json!({"tab":4})).is_err());
        assert_eq!(
            decode::<ChromeEffect>("pan", serde_json::json!({"strip":"9","offset":1.5})).ok(),
            Some(ChromeEffect::Pan {
                strip: "9".into(),
                offset: 1.5
            })
        );
    }

    #[test]
    fn a_window_of_one_tab_has_nothing_to_split() {
        let mut tree = tree();
        let split = chrome("split-right", serde_json::json!({"tab":"4"}));
        assert_eq!(
            admit(&split, &tree).unwrap_err(),
            t("window-menu-split-one-tab")
        );
        // A second tab makes the window one a split has something to take
        // out of: the same chord is admitted, and the same rule keeps the
        // menu's row.
        let group = &mut tree.workspaces[0].strips[0].window_groups[0];
        group.tabs.push(Tab {
            id: 5,
            title: "Another".into(),
            ..group.tabs[0].clone()
        });
        let ids = tab_route(&tree, "4").unwrap();
        assert!(has_a_second_tab(&tree, &ids));
        assert!(matches!(admit(&split, &tree), Ok(Admitted::SplitRight(_))));
    }

    #[test]
    fn route_round_trips_full_u64_and_rejects_numeric_ids() {
        let route = Route {
            workspace: u64::MAX.to_string(),
            strip: "9007199254740993".into(),
            group: "3".into(),
            tab: "4".into(),
        };
        assert_eq!(route.ids().unwrap().workspace.0, u64::MAX);
        assert_eq!(route.ids().unwrap().strip.0, 9_007_199_254_740_993);
        assert_eq!(
            serde_json::from_str::<Route>(&serde_json::to_string(&route).unwrap()).unwrap(),
            route
        );
        assert!(
            serde_json::from_str::<Route>(r#"{"workspace":1,"strip":"2","group":"3","tab":"4"}"#)
                .is_err()
        );
        assert!(
            Route {
                tab: "04".into(),
                ..route
            }
            .ids()
            .is_err()
        );
    }

    #[test]
    fn snapshot_names_the_active_window_and_admits_only_live_routes() {
        let tree = tree();
        let snapshot = snapshot(&tree, "g1", vec![]);
        let active = snapshot.active.clone().unwrap();
        assert_eq!(active.workspace, u64::MAX.to_string());
        assert_eq!(active.tab, "4");
        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(
            snapshot.width_permille,
            Some(WindowWidth::Normal.permille())
        );
        let json = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(json["activeStrip"], "9007199254740993");
        assert_eq!(json["capabilities"]["workspaceSelect"], true);

        let select = desk("focus", serde_json::json!({"tab":"4"}));
        assert!(matches!(admit(&select, &tree), Ok(Admitted::Select(_))));
        let gone = desk("focus", serde_json::json!({"tab":"5"}));
        assert_eq!(admit(&gone, &tree).unwrap_err(), "Window unavailable");
        let bad: Result<DeskEffect, String> = decode("focus", serde_json::json!({"tab":"four"}));
        assert_eq!(
            bad.unwrap_err(),
            "the effect's `tab` argument is `four`, which names no id: ids travel as decimal \
             text"
        );
        let search = desk("search", serde_json::json!({}));
        assert_eq!(admit(&search, &tree), Ok(Admitted::Search(None)));
        let typed = desk("search", serde_json::json!({"query":"sheets"}));
        assert_eq!(
            admit(&typed, &tree),
            Ok(Admitted::Search(Some("sheets".into())))
        );
        assert_eq!(json["capabilities"]["globalSearch"], true);
    }

    #[test]
    fn pan_is_admitted_only_for_the_shown_strip_at_a_finite_offset() {
        let tree = tree();
        let s = "9007199254740993";
        assert_eq!(
            admitted(
                &tree,
                &chrome("pan", serde_json::json!({"strip":s,"offset":120.5}))
            ),
            Ok(Admitted::Pan(StripId(9_007_199_254_740_993), 120.5))
        );
        assert_eq!(
            admitted(
                &tree,
                &chrome("pan", serde_json::json!({"strip":"8","offset":0}))
            )
            .unwrap_err(),
            "Strip is no longer active"
        );
        assert_eq!(
            admitted(
                &tree,
                &chrome("pan", serde_json::json!({"strip":s,"offset":0}))
            ),
            Ok(Admitted::Pan(StripId(9_007_199_254_740_993), 0.0))
        );
        let infinite = ChromeEffect::Pan {
            strip: s.into(),
            offset: f64::INFINITY,
        };
        assert_eq!(
            admit(&infinite.into(), &tree).unwrap_err(),
            "Invalid pan offset"
        );
    }

    #[test]
    fn scroll_and_history_name_a_live_window() {
        let tree = tree();
        let active = active_route(&tree).unwrap();
        let region = scroll_region(active, "primary");
        assert_eq!(
            admitted(
                &tree,
                &chrome(
                    "scroll",
                    serde_json::json!({"region":region,"action":{"kind":"page","value":-1}})
                )
            ),
            Ok(Admitted::Scroll(active, ScrollAction::Page(-1)))
        );
        let gone = scroll_region(RouteIds { tab: 99, ..active }, "primary");
        assert_eq!(
            admitted(
                &tree,
                &chrome(
                    "scroll",
                    serde_json::json!({"region":gone,"action":{"kind":"end"}})
                )
            )
            .unwrap_err(),
            "Scroll target is no longer active"
        );
        assert!(
            admitted(
                &tree,
                &chrome(
                    "scroll",
                    serde_json::json!({"region":"nonsense","action":{"kind":"end"}})
                )
            )
            .is_err()
        );
        assert_eq!(
            admitted(
                &tree,
                &chrome("history", serde_json::json!({"tab":"4","target":"forward"}))
            ),
            Ok(Admitted::History(active, HistoryDirection::Forward))
        );
        assert_eq!(
            admitted(
                &tree,
                &chrome("history", serde_json::json!({"tab":"4","target":"up"}))
            )
            .unwrap_err(),
            "Unknown history target"
        );
    }

    #[test]
    fn activity_views_need_history_and_the_active_workspace() {
        let mut tree = tree();
        let w = u64::MAX.to_string();
        for view in ["catch-up", "explore", "replay"] {
            let reason = admitted(
                &tree,
                &chrome("activity", serde_json::json!({"workspace":w,"view":view})),
            )
            .unwrap_err();
            assert!(
                reason.contains("history, which this desktop does not carry"),
                "{view}: {reason}"
            );
        }
        tree.workspaces
            .push(Workspace::new(WorkspaceSlot(2), "Other"));
        let capabilities = Presentation {
            retained_history: true,
            ..Presentation::default()
        }
        .capabilities();
        let elsewhere = chrome(
            "activity",
            serde_json::json!({"workspace":"2","view":"explore"}),
        );
        assert_eq!(
            admit_with(&elsewhere, &tree, &capabilities).unwrap_err(),
            "Workspace is no longer active"
        );
        assert_eq!(
            admitted(
                &tree,
                &chrome("close-activity", serde_json::json!({"workspace":w}))
            ),
            Ok(Admitted::CloseActivity(WorkspaceSlot(u64::MAX)))
        );
        let caps =
            serde_json::to_value(snapshot(&tree, "g", vec![])).unwrap()["capabilities"].clone();
        assert_eq!(caps["swipe"], true);
        assert_eq!(caps["workspaceHistory"], false);
        assert_eq!(caps["workspaceReplay"], false);
        assert_eq!(
            (
                caps["pan"].clone(),
                caps["scroll"].clone(),
                caps["history"].clone()
            ),
            (true.into(), true.into(), true.into())
        );
    }

    #[test]
    fn a_history_source_admits_every_view_and_the_snapshot_says_so() {
        let tree = tree();
        let w = u64::MAX.to_string();
        let presentation = Presentation {
            retained_history: true,
            notifications: Some(NotificationsState::default()),
            ..Presentation::default()
        };
        let capabilities = presentation.capabilities();
        for (wire, view) in [
            ("catch-up", ActivityView::CatchUp),
            ("explore", ActivityView::Explore),
            ("replay", ActivityView::Replay),
        ] {
            let verb = chrome("activity", serde_json::json!({"workspace":w,"view":wire}));
            assert_eq!(
                admit_with(&verb, &tree, &capabilities),
                Ok(Admitted::Activity(WorkspaceSlot(u64::MAX), view, None))
            );
        }
        let center = chrome("notifications", serde_json::json!({"open":true}));
        assert_eq!(
            admit_with(&center, &tree, &capabilities),
            Ok(Admitted::NotificationCenter(true))
        );
        assert_eq!(
            admit(&center, &tree).unwrap_err(),
            "Notifications are unavailable on this desktop"
        );
        let json = serde_json::to_value(snapshot_with(&tree, "g", vec![], &presentation)).unwrap();
        assert_eq!(json["capabilities"]["workspaceHistory"], true);
        assert_eq!(json["capabilities"]["workspaceReplay"], true);
        assert_eq!(json["capabilities"]["notifications"], true);
        assert_eq!(json["notifications"]["retained"], 0);
    }

    #[test]
    fn stale_presentation_is_left_out_of_the_snapshot() {
        let tree = tree();
        let active = active_route(&tree).unwrap();
        let presentation = Presentation {
            pan: Some(DesktopPan {
                workspace: u64::MAX.to_string(),
                strip: "1".into(),
                offset: 0.0,
                viewport: 1.0,
                content: 2.0,
                max: 1.0,
            }),
            scroll: Some(ScrollState {
                region: scroll_region(RouteIds { tab: 7, ..active }, "primary"),
                offset: 0.0,
                viewport: 1.0,
                content: 2.0,
                max: 1.0,
            }),
            history: Some(HistoryState {
                can_back: true,
                can_forward: false,
            }),
            ..Presentation::default()
        };
        let json = serde_json::to_value(snapshot_with(&tree, "g", vec![], &presentation)).unwrap();
        assert!(json["desktopPan"].is_null(), "another strip");
        assert!(json["scroll"].is_null(), "another window");
        assert_eq!(json["history"]["canBack"], true);
        assert_eq!(json["history"]["canForward"], false);
    }

    #[test]
    fn launch_names_an_application_on_the_shown_strip() {
        let tree = tree();
        let launch = chrome("open-app", serde_json::json!({"app":"so.tilo.notes"}));
        assert_eq!(
            admit(&launch, &tree),
            Ok(Admitted::Launch("so.tilo.notes".into()))
        );
        let empty = chrome("open-app", serde_json::json!({"app":""}));
        assert!(admit(&empty, &tree).is_err());
        assert_eq!(
            serde_json::to_value(snapshot(&tree, "g1", vec![])).unwrap()["capabilities"]["launch"],
            true
        );
    }

    #[test]
    fn open_focuses_the_shown_window_of_an_app_or_launches_one() {
        let tree = tree();
        let open = |app: &str| chrome("show-app", serde_json::json!({"app":app}));
        let shown = RouteIds {
            workspace: WorkspaceSlot(u64::MAX),
            strip: StripId(9_007_199_254_740_993),
            group: 3,
            tab: 4,
        };
        assert_eq!(
            admit(&open("so.tilo.notes"), &tree),
            Ok(Admitted::Select(shown))
        );
        assert_eq!(
            admit(&open("so.tilo.chat"), &tree),
            Ok(Admitted::Open("so.tilo.chat".into()))
        );
        assert!(admit(&open(""), &tree).is_err());
        let json = serde_json::to_value(snapshot(&tree, "g1", vec![])).unwrap();
        assert_eq!(json["entries"][0]["app"], "so.tilo.notes");
    }
}
