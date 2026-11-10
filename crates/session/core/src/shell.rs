//! What a shell reads of the session and asks it: the desk's reading, the resources the session
//! publishes it through, and the screen's edges — the same on every host, so a shell written once
//! runs on each.
//!
//! The session publishes the desk as [`DESK_RESOURCE`]'s reading ([`DeskReading`]); a shell's views
//! read it, and ask the session for the rest through [`Ask`]s on the door.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The origin the session's resources come from: the first part of their kinds.
pub const ORIGIN: &str = "session";
/// The resource the person's arrangement is published on: what a composed view's open becomes a
/// place on (`place`).
pub use crate::arrangement::RESOURCE as ARRANGEMENT_RESOURCE;
/// The resource the session's chrome actions are published on.
pub use tilo_desktop_search::CHROME_RESOURCE;
/// The resource the session publishes its desk as, whose reading a shell's origin writes: the
/// door's own name for it, so the session, its broker and its views name one desk.
pub use ui::tilo_actions::remote::session_actions::DESK_RESOURCE;

/// What stands over the windows, and in what order, in the session's catalogue: the surface stack.
pub const OVERLAYS: &str = "desktop:overlays";
/// The resource the session publishes of its displays.
pub const DISPLAYS_RESOURCE: &str = "session:displays";

/// The shell's own record: the package's named root `shell` (`root shell: Shell`), reached in
/// its views as `shell`.
pub const SHELL_RECORD: &str = "shell";

/// The package id of the shell a device installs, and the one this build's fallback copy is.
pub const SHELL_APP: &str = "so.tilo.shell";

/// A workspace as the desk's reading names it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceReading {
    pub key: String,
    pub name: String,
    pub current: bool,
}

/// Someone else here, as the desk's reading names them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonReading {
    pub key: String,
    pub name: String,
}

/// A window of the desk: a tab of a strip of one of its workspaces, as the desk's reading
/// names it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WindowReading {
    pub key: String,
    pub title: String,
    pub origin_app: String,
    /// The app it shows, as two windows of one app share it: the walk's app scope crosses
    /// the windows holding it.
    pub app_key: String,
    pub icon: String,
    pub strip: String,
    /// Its strip's id, as moving a window there names the strip.
    pub strip_key: String,
    pub workspace: String,
    /// Its workspace's id, as switching to it names the workspace.
    pub workspace_key: String,
    /// The window it is a tab of: a group's tabs are one window on the desk, and its other
    /// tabs stand behind it.
    pub group_key: String,
    /// Its place among its group's tabs: `0` for the tab in front, then the tabs behind it in
    /// their order.
    pub tab_index: i64,
    /// Its share of its strip's width, 0..1: the width the window keeps on the desk, so a
    /// picture of the strip keeps the desk's proportions.
    pub share: f64,
    /// Whether it runs on this device: false for a window on another of the person's devices,
    /// or on a peer's.
    pub local: bool,
    /// The device that runs it, when it is not this one: its hex device key, empty when the
    /// device is not known.
    pub device: String,
    /// That device's name, as its certificate labels it.
    pub device_name: String,
    /// Where it stands in the picture of its workspace: 1 for the first tab of the first group
    /// of the first strip, on along them, strips top to bottom and groups leading to trailing.
    pub place: i64,
    /// Where it stands in the walk Alt+Tab takes: 1 is the window in front, then the ones it
    /// was in front of, then the rest along their strips; windows of the other workspaces
    /// come after them, as the walk crosses the workspace in front alone.
    pub recency: i64,
    pub focused: bool,
    /// Whether it may be pictured: `live`, `withheld` (it holds secrets) or `none`.
    pub preview: String,
    /// Whether its app stopped answering the compositor's pings: its window says so, and
    /// offers to wait for the app or quit it.
    #[serde(default)]
    pub not_responding: bool,
}

/// An agent here for the person, or that acted for them, as the desk's reading names it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentReading {
    pub key: String,
    pub name: String,
    /// `running`, `waiting` (for the person) or `done`.
    pub state: String,
    pub doing: String,
    pub mark: String,
    pub unseen: i64,
    /// Whether it is seated at the person's agent door now, a client they admitted, rather
    /// than a run's own agent.
    #[serde(default)]
    pub seat: bool,
}

/// A workspace the person is in, as the workspace overview names it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomReading {
    pub key: String,
    pub name: String,
    pub current: bool,
    pub members: String,
    pub present: i64,
    pub activity: String,
}

/// Something a sender told the person here: an app, or the agent task that did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoticeReading {
    pub key: String,
    pub title: String,
    pub body: String,
    pub origin_app: String,
    /// `info`, `success`, `warning` or `error`.
    pub kind: String,
    pub seen: bool,
}

/// One action a notice offers, as its sender named it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoticeActionReading {
    pub notice: String,
    pub key: String,
    pub label: String,
}

/// A timer or stopwatch running in Clocks, as the bar's clock reads it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimerReading {
    /// The activity's id, which a press on the clock's controls names.
    #[serde(default)]
    pub activity_id: Option<String>,
    /// Its label, such as `Tea`.
    pub label: String,
    /// What the timer has left, or the stopwatch has run, in seconds.
    pub remaining_secs: u64,
    #[serde(default)]
    pub stopwatch: bool,
    #[serde(default)]
    pub paused: bool,
}

/// The reading of [`DESK_RESOURCE`]: the fields of the `session.desk` kind
/// (`tilolang::origin::KINDS`), as JSON on the door.
#[allow(
    clippy::struct_excessive_bools,
    reason = "the desk's reading is its fields, each read as the shell needs it"
)]
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DeskReading {
    pub person: String,
    pub clock: String,
    /// The timer or stopwatch Clocks shows, when one runs: what the bar's clock reads beside
    /// the time, and the controls pressed on it name.
    #[serde(default)]
    pub timer: Option<TimerReading>,
    /// The question the session puts to the person about themselves, waiting on their own
    /// avatar until they answer it or put it off.
    #[serde(default)]
    pub ask: Option<AskReading>,
    pub weekday: String,
    pub date: String,
    pub quiet: bool,
    pub workspace: String,
    pub strip: String,
    pub workspaces: Vec<WorkspaceReading>,
    /// The surface the desk shows over its windows: `none`, `desk`, `overview`, `switcher`,
    /// `launcher`, `centre`, `workspaces` or `composer`.
    #[serde(default)]
    pub here: String,
    /// Whether the switcher's panel shows: Alt has been held on the walk past the flick that
    /// only switches windows, so a quick Alt+Tab draws nothing.
    #[serde(default)]
    pub switcher_revealed: bool,
    /// Which windows the switcher's walk crosses: `workspace` for every window of the
    /// workspace (Alt+Tab), or `app` for the ones the window in front's app shows (Alt and the
    /// grave key).
    #[serde(default)]
    pub switcher_scope: String,
    /// What the person last typed in the shell's search.
    #[serde(default)]
    pub spotlight: String,
    /// The notice shown as a banner over the desk now, by its id; empty for none.
    #[serde(default)]
    pub banner: String,
    /// The windows of every strip of the workspace in front, most recently used first.
    #[serde(default)]
    pub windows: Vec<WindowReading>,
    /// The person's agents here, and what each is on and did.
    #[serde(default)]
    pub agents: Vec<AgentReading>,
    /// The workspaces the person is in, with who is in each and what it holds.
    #[serde(default)]
    pub rooms: Vec<RoomReading>,
    /// What the person was told, newest first.
    #[serde(default)]
    pub notices: Vec<NoticeReading>,
    /// Each action a notice offers, naming the notice it is on, in its sender's order.
    #[serde(default)]
    pub notice_actions: Vec<NoticeActionReading>,
    /// Each app's unread count as its badge shows it, by app id: read on the door (a dock, an
    /// agent); no `session.desk` field holds it yet.
    #[serde(default)]
    pub badges: BTreeMap<String, u32>,
    /// The others here in the workspace in front: its collaborators.
    #[serde(default)]
    pub people: Vec<PersonReading>,
    /// Whether the pointer is at, or resting against, the top edge of the output: a shell's bar
    /// that hides until it is reached for shows while this holds.
    #[serde(default)]
    pub bar_reached: bool,
    /// Whether a window stands where a top bar rides: a bar that hides while a window overlaps
    /// it hides while this holds.
    #[serde(default)]
    pub bar_covered: bool,
    /// Whether the pointer is at the bottom edge of the output: a dock there shows while this
    /// holds.
    #[serde(default)]
    pub dock_reached: bool,
    /// Whether a window stands where a dock rides.
    #[serde(default)]
    pub dock_covered: bool,
    /// The running applications' tray items, in the order they registered.
    #[serde(default)]
    pub tray: Vec<TrayReading>,
    /// The desk styles every installed package offers, as the session finds them (shell.md
    /// §6): each a view of the arrangement, listed by its own doc line, so the person's switch
    /// offers every style wherever it was written.
    #[serde(default)]
    pub styles: Vec<DeskStyleReading>,
}

/// A desk style, as the desk's reading names it: a view of the arrangement an installed
/// package declares (shell.md §6).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeskStyleReading {
    /// The view reference: a package and a view (`so.tilo.shell:strips`), as `Viewing.style`
    /// names it.
    pub style: String,
    /// The package that declares it.
    pub package: String,
    /// The view's own name, as its declaration writes it.
    pub name: String,
    /// The view's doc line, in its author's words: what the switch lists it by.
    pub line: String,
    /// Whether the style loads: false for one whose package does not compile against the shell
    /// and the session, or whose view failed when it ran, so the switch can say so and the desk
    /// shows the built-in strips until it is fixed (`shell.md` §6).
    pub loads: bool,
    /// Why the style does not load, in the runtime's words, when it does not; empty when it
    /// does.
    #[serde(default)]
    pub reason: String,
}

/// The arrangement's readings, which a shell reads beside the desk's: what the person arranged.
pub use crate::arrangement::{
    ArrangementPlaceReading, ArrangementReading, ArrangementRowReading, ArrangementStripReading,
    ArrangementWorkspaceReading,
};

/// A running application's tray item, as the bar shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrayReading {
    /// The item's key, which a press on it names.
    pub key: String,
    /// The app's own words for it.
    pub title: String,
    /// Whether choosing it opens its menu rather than its window.
    #[serde(default)]
    pub menu: bool,
}

/// A question the session puts to the person about themselves, waiting on their own avatar.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskReading {
    /// The question ("Keep your name and work?").
    pub question: String,
    /// What answering brings.
    pub detail: String,
    /// The answer's label ("Keep with passkey").
    pub answer: String,
    /// Shown as the popover; `false` while it waits as the avatar's dot.
    pub open: bool,
}

impl DeskReading {
    /// The timer Clocks runs, by its name: its label (`Tea`), or what it is when the app
    /// named none (`Timer`, `Stopwatch`). Empty when no timer floats.
    #[must_use]
    pub fn timer_name(&self) -> String {
        let Some(timer) = &self.timer else {
            return String::new();
        };
        if !timer.label.is_empty() {
            return timer.label.clone();
        }
        if timer.stopwatch {
            "Stopwatch".to_owned()
        } else {
            "Timer".to_owned()
        }
    }

    /// What the timer reads: what it has left, or what the stopwatch has run.
    #[must_use]
    pub fn timer_left(&self) -> String {
        self.timer.as_ref().map_or_else(String::new, |timer| {
            ui::format::timecode(timer.remaining_secs)
        })
    }
}

/// What a shell asked of the session: an invocation on the door.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Ask {
    pub resource: String,
    pub action: String,
    pub arguments: BTreeMap<String, String>,
    /// The resource the invocation is made from, when it is made from one.
    pub from: Option<ui::tilo_actions::params::MadeFrom>,
}

/// How far from an output's edge the pointer resting there counts as at it: a bar or dock
/// pinned to a hiding edge is reached for by coming within this of that edge. The reach pad
/// the desk's own widgets use, so a glove and a mouse reach the same distance.
#[must_use]
pub fn reach_pad() -> f64 {
    f64::from(ui::tokens::Spacing::BASE.s2)
}

/// A window as the compositor shows it, for what the chrome needs of it: where it stands.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl WindowRect {
    fn covers(&self, edge_top: bool, band: f64, outputs: (i32, i32, u32, u32)) -> bool {
        let (x0, _, width, height) = outputs;
        let (left, right) = (f64::from(x0), f64::from(x0) + f64::from(width));
        let horizontal = self.x < right && self.x + self.width > left;
        if !horizontal {
            return false;
        }
        if edge_top {
            // A window reaching into the top band covers a bar there.
            let top = f64::from(outputs.1);
            self.y < top + band && self.y + self.height > top
        } else {
            // A window reaching into the bottom band covers a dock there.
            let bottom = f64::from(outputs.1) + f64::from(height);
            self.y < bottom && self.y + self.height > bottom - band
        }
    }
}

/// What the screen says of the edges a bar or dock that shows only sometimes rides: whether
/// the pointer is at each edge, and whether a window stands where a bar or dock there would.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each edge's own facts, read one at a time"
)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScreenEdges {
    pub bar_reached: bool,
    pub bar_covered: bool,
    pub dock_reached: bool,
    pub dock_covered: bool,
}

/// What a screen says of its edges: the pointer at `pointer` and the windows `windows`, over
/// `outputs` (their bounding box, `(x, y, w, h)`), for a bar and a dock of `band` logical
/// pixels. The pointer is at an edge when it rests within [`reach_pad`] of the top or bottom
/// output edge there, or over the band a shown bar or dock would take.
#[must_use]
pub fn screen_edges(
    outputs: (i32, i32, u32, u32),
    band: f64,
    pointer: (f64, f64),
    windows: &[WindowRect],
) -> ScreenEdges {
    let (x0, y0, width, height) = outputs;
    let (x, y) = pointer;
    let (left, right) = (f64::from(x0), f64::from(x0) + f64::from(width));
    let (top, bottom) = (f64::from(y0), f64::from(y0) + f64::from(height));
    let over = x >= left && x < right;
    let pad = reach_pad();
    let at_top = over && y >= top - pad && y <= top + pad;
    let at_bottom = over && y <= bottom + pad && y >= bottom - pad;
    ScreenEdges {
        // A bar reached for is reached at its own edge, or by the pointer coming to rest on
        // the band it shows in.
        bar_reached: at_top || (over && y >= top - pad && y <= top + band),
        dock_reached: at_bottom || (over && y <= bottom + pad && y >= bottom - band),
        bar_covered: windows.iter().any(|w| w.covers(true, band, outputs)),
        dock_covered: windows.iter().any(|w| w.covers(false, band, outputs)),
    }
}
