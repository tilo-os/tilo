//! The layout's windows as a reading takes them.

use serde::{Deserialize, Serialize};

/// The layout's windows as `session:windows` publishes them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct Windows {
    /// Every workspace's windows, in the order Alt+Tab walks them.
    pub all: Vec<Window>,
    /// The displays the desktop spreads over, in reading order: each by the id a window moves to
    /// it with and the number a person counts it by.
    pub displays: Vec<WindowDisplay>,
}

/// One of the displays the desktop spreads over, as moving a window there names it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct WindowDisplay {
    /// The display's id, as `desktop:chrome move-tab` names the display a window goes to.
    pub id: String,
    /// The number a person counts it by, from one, left to right.
    pub number: i64,
}

/// A window of the layout: a tab of a strip of one of its workspaces.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct Window {
    pub key: String,
    pub title: String,
    /// The app's name, as people know it.
    pub origin_app: String,
    /// The app's id: the switcher's app scope crosses the windows that share it.
    pub app_key: String,
    pub icon: String,
    pub strip: String,
    /// Its strip's id, as moving a window there names the strip.
    pub strip_key: String,
    pub workspace: String,
    /// Its workspace's id, as switching to it names the workspace.
    pub workspace_key: String,
    /// The window it is a tab of: a group's other tabs stand behind it.
    pub group_key: String,
    /// Its place among its group's tabs: `0` for the tab in front.
    pub tab_index: i64,
    /// Its share of its strip's width, 0..1.
    pub share: f64,
    /// Whether it runs on another device: a window is this device's unless it says so.
    pub elsewhere: bool,
    /// The device that runs it, when it is not this one: its hex key, empty when not known.
    pub device: String,
    /// That device's name, as its certificate labels it.
    pub device_name: String,
    /// Where it stands in the picture of its workspace: 1 for the first tab of the first group
    /// of the first strip, on along them.
    pub place: i64,
    /// Where it stands in the walk Alt+Tab takes: 1 is the window in front.
    pub recency: i64,
    pub focused: bool,
    /// Whether it may be pictured: `live`, `withheld` (it holds secrets) or `none`.
    pub preview: String,
    /// Whether its app stopped answering the compositor's pings.
    pub not_responding: bool,
    /// Whether its window covers its display.
    pub fullscreen: bool,
    /// Whether its window floats above its workspace's windows, rather than standing in its strip.
    pub floating: bool,
    /// Whether the person may name its window: a host window's own title, never an app's resource.
    pub renameable: bool,
    /// Whether its app opens another tab: a foreign or local app's window, never a placeholder
    /// or a saved window a tab stands in for.
    pub opens_tabs: bool,
    /// Whether its app controls this desktop beyond its window now.
    pub controls_desktop: bool,
    /// The display it stands on, by the id [`WindowDisplay::id`] names, empty while no display
    /// shows it.
    pub display_id: String,
    /// What the app calls what its window shows, as its bar's location: a note's title, a
    /// folder's path. Empty when the app names nothing there.
    pub bar_location: String,
    /// The field its bar holds open: `location` (the app asked to edit what it shows), `rename`
    /// (the person is naming the window), `find` or `replace` (the app's search), or empty.
    pub bar_field: String,
    /// Whether a field in its bar takes what is typed now, holding the keys off the window.
    pub bar_typing: bool,
    /// The parts its app declared for its bar, when it declared any.
    pub bar: Option<Bar>,
    /// Who opened it, person or agent, when the workspace's shared desktop says.
    pub author: Option<Author>,
    /// What it says in place of its app, when the app could not open or stopped.
    pub ended: Option<Ended>,
}

/// What a window says in the place its app left, when the app could not open or stopped: the
/// outcome with the way to ask for it again.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct Ended {
    /// The sentence it leads with, in the person's words.
    pub what: String,
    /// Why, in words they can act on; empty where what happened is all there is to say.
    pub why: String,
    /// What the app itself said of it, quoted under the sentence; empty where it said nothing
    /// that tells.
    pub said: String,
    /// The label of the button that asks for the app again, when asking can work.
    pub retry: Option<String>,
    /// The technical side, kept behind the card's Details.
    pub details: String,
}

/// Who opened a window: the member whose hands opened it, person or agent, and the agent's run
/// when one did. The window's bar says whose it is; what it may be done to is the workspace's
/// permissions, never this.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct Author {
    /// The member's id: a person's, or the agent's member id when an agent opened it.
    pub identity: String,
    /// `person` or `agent`.
    pub kind: String,
    /// What they are called, when the one who opened it set a label: the agent's name.
    /// Empty for a person, whose name is the workspace's to say.
    pub name: String,
    /// The presence colour they wear, as `#rrggbb`: the one every surface draws them in.
    pub colour: String,
    /// The agent session whose run opened the window, when one did.
    pub agent: String,
    /// That run, as History groups its changes.
    pub run: String,
    /// What the agent called the step that opened the window; empty when it titled none.
    pub title: String,
}

/// The parts an app declares for its window's bar, as the shell's views draw them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct Bar {
    /// Its controls before the location: the app's own.
    pub leading: Vec<BarAction>,
    /// Its actions, in its own order.
    pub actions: Vec<BarAction>,
    /// Whether it can go back, and forward, through what it showed.
    pub back: bool,
    pub forward: bool,
    /// Whether it offers a sidebar toggle, and whether the sidebar shows now.
    pub sidebar: bool,
    pub sidebar_on: bool,
    /// The views it switches between, when it shows more than one.
    pub views: Option<BarViews>,
    /// What it says to the person in its bar, when something needs them.
    pub notice: Option<BarNotice>,
    /// The reactions to the one thing it shows.
    pub reactions: Option<BarReactions>,
}

/// One control of a window's bar.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct BarAction {
    /// The action id its app knows it by.
    pub id: String,
    pub name: String,
    pub icon: String,
    pub enabled: bool,
    /// Whether it reads on: a toggle that is set.
    pub on: bool,
    /// Whether it is a toggle.
    pub toggles: bool,
    /// Whether it stands in the bar's overflow menu only.
    pub menu: bool,
    /// What a press lists, for a menu button: each chosen by its id.
    pub items: Vec<BarAction>,
}

/// The views a window's bar switches between.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct BarViews {
    /// What the switch is called.
    pub name: String,
    pub choices: Vec<BarView>,
    /// The view shown, by its id.
    pub chosen: String,
}

/// One view a window's bar offers.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct BarView {
    pub id: String,
    pub label: String,
}

/// What a window says in its bar: a line, its tone, and up to two actions beside it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct BarNotice {
    pub text: String,
    /// `info`, `success`, `warning` or `error`, as its colour reads it.
    pub tone: String,
    pub actions: Vec<BarAction>,
}

/// The reactions to what a window shows, as its bar draws them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct BarReactions {
    pub pills: Vec<BarReaction>,
}

/// One emoji's reactions.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct BarReaction {
    pub emoji: String,
    pub count: i64,
    /// Whether the person here is among them.
    pub mine: bool,
    /// Who reacted, as their names read.
    pub names: Vec<String>,
    /// The action id a press on the pill sends its app.
    pub action: String,
}
