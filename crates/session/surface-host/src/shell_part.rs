//! A part of a tilolang shell (its top bar first) shown on the desktop.
//!
//! A shell package's views run in a tilolang instance (`tilolang_tilo::instance`), exactly as
//! an app's do, and draw through the same UI host half (`tilo_ui_host::AppWindow`, here as a
//! [`AppWindow::part`]: the view as it is, no window shell). The toolkit lays that widget tree
//! out and the CPU rasterizes it, as [`WidgetHost`] does for every widget tree the session
//! shows; the compositor gets the buffer as a layer. Nothing here renders on its own.
//!
//! What the views read of the session comes in as resources whose origin is the session
//! (`type Desk (origin: session.desk)`): [`SessionOrigin`] writes what the session publishes on
//! its door (the `session:desk` resource's reading) into them, and carries the shell's outside
//! effects (`outside("session.act", ...)`) back to the door as invocations. A shell view never
//! reaches the session another way.
//!
//! The part works only when something changed: a press, a reading of the session's that moved,
//! or a door answer. Otherwise a frame asks nothing of the instance, and the toolkit draws
//! nothing.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use tilo_compositor::{CursorIcon, HostKey, HostLayer, KeyClaim, SurfaceHost};
use tilo_render_protocol::patch::local::pair;
use tilo_ui_host::AppWindow;
use tilolang_tilo::background::Waker;
use tilolang_tilo::host::{Host, RootView};
use tilolang_tilo::instance::{Config, Instance, Mode, Platform};
use tilolang_tilo::origin::{Origin, OriginCx, OriginType, Polled, origin_types};
use tilolang_tilo::publish::{COMMIT, HOST_RESOURCE};
use tilolang_tilo::runtime::origin::OriginWrite;
use tilolang_tilo::runtime::{Delivery, NameHost, Principal, ResourceRef, Value};
use tilolang_tilo::tilolang::Compiled;
use ui::tilo_actions::Action as _;

use crate::{Placement, WidgetHost};

/// The origin the session's resources come from: the first part of their kinds.
pub const ORIGIN: &str = "session";
/// The resource the session publishes its desk as, whose reading [`SessionOrigin`] writes.
pub const DESK_RESOURCE: &str = "session:desk";
/// Its action that brings a workspace to the front, by key.
pub const SWITCH_WORKSPACE: &str = "switch-workspace";
/// The argument of [`SWITCH_WORKSPACE`] naming the workspace.
pub const WORKSPACE_ARG: &str = "workspace";
/// The desk's action that opens or closes a running application's tray item, by its key.
pub const OPEN_TRAY_ITEM: &str = "open-tray-item";
/// The argument of [`OPEN_TRAY_ITEM`] naming the tray item.
pub const TRAY_KEY_ARG: &str = "key";
/// The resource the notification centre's actions are published on.
pub const NOTIFICATIONS_RESOURCE: &str = "quick-settings.panel";

/// The resource whose argument-less actions `session.act` names.
pub const CHROME_RESOURCE: &str = "desktop:chrome";

/// The capability that switches workspace: its payload is the workspace's key.
const CAP_SWITCH: &str = "session.switch_workspace";
/// The capability that runs one of the desktop's own argument-less actions, by id.
const CAP_ACT: &str = "session.act";
/// The capability that runs one of the desktop's own actions that take arguments.
const CAP_RUN: &str = "session.run";
/// The capability that runs one of the notification centre's actions: the same ask as
/// [`CAP_RUN`], on the panel's resource.
const CAP_NOTIFY: &str = "session.notify";
/// The capability that arranges the person's desk: its payload is the published action id and
/// its arguments, as one line of JSON (`move-place {"place":3,"row":7}`). One capability keeps
/// the seam small while the arranging actions are a closed set the session publishes.
const CAP_ARRANGE: &str = "session.arrange";
/// The resource the person's arrangement is published on, whose actions `session.arrange` names.
pub const ARRANGEMENT_RESOURCE: &str = "shell:arrangement";

/// The shell's own record: the package's named root `shell` (`root shell: Shell`), reached in
/// its views as `shell`.
pub const SHELL_RECORD: &str = "shell";

/// Every file of the shell's package (`apps/tl/shell`), by the name the package knows it by: a
/// package is a directory of `.tl` files, in path order (the package's compiler reads them so),
/// and each new one is listed here to ship.
const SHELL_FILES: &[(&str, &str)] = &[
    ("app.tl", include_str!("../../../../apps/tl/shell/app.tl")),
    (
        "arrangement.tl",
        include_str!("../../../../apps/tl/shell/arrangement.tl"),
    ),
    (
        "arranging.tl",
        include_str!("../../../../apps/tl/shell/arranging.tl"),
    ),
    (
        "canvas.tl",
        include_str!("../../../../apps/tl/shell/canvas.tl"),
    ),
    (
        "chrome.tl",
        include_str!("../../../../apps/tl/shell/chrome.tl"),
    ),
    (
        "composer.tl",
        include_str!("../../../../apps/tl/shell/composer.tl"),
    ),
    ("desk.tl", include_str!("../../../../apps/tl/shell/desk.tl")),
    (
        "docks.tl",
        include_str!("../../../../apps/tl/shell/docks.tl"),
    ),
    (
        "launcher.tl",
        include_str!("../../../../apps/tl/shell/launcher.tl"),
    ),
    (
        "layout.tl",
        include_str!("../../../../apps/tl/shell/layout.tl"),
    ),
    (
        "notices.tl",
        include_str!("../../../../apps/tl/shell/notices.tl"),
    ),
    (
        "overview.tl",
        include_str!("../../../../apps/tl/shell/overview.tl"),
    ),
    (
        "parts.tl",
        include_str!("../../../../apps/tl/shell/parts.tl"),
    ),
    (
        "people.tl",
        include_str!("../../../../apps/tl/shell/people.tl"),
    ),
    (
        "presets.tl",
        include_str!("../../../../apps/tl/shell/presets.tl"),
    ),
    (
        "rooms.tl",
        include_str!("../../../../apps/tl/shell/rooms.tl"),
    ),
    (
        "scenarios.tl",
        include_str!("../../../../apps/tl/shell/scenarios.tl"),
    ),
    (
        "session.tl",
        include_str!("../../../../apps/tl/shell/session.tl"),
    ),
    (
        "strips.tl",
        include_str!("../../../../apps/tl/shell/strips.tl"),
    ),
    (
        "switcher.tl",
        include_str!("../../../../apps/tl/shell/switcher.tl"),
    ),
    (
        "system.tl",
        include_str!("../../../../apps/tl/shell/system.tl"),
    ),
    (
        "traditional.tl",
        include_str!("../../../../apps/tl/shell/traditional.tl"),
    ),
];
/// The shell package's ids lock.
const SHELL_IDS: &str = include_str!("../../../../apps/tl/shell/app.ids");

/// The packages the shell's views reach (`notes`, `tasks`, `chat`), each as its source and ids
/// lock, in the order they compile: a package comes after those it imports.
const SHELL_USES: &[(&str, &str, &str)] = &[
    (
        "chat",
        include_str!("../../../../apps/tl/chat/app.tl"),
        include_str!("../../../../apps/tl/chat/app.ids"),
    ),
    (
        "notes",
        include_str!("../../../../apps/tl/notes/app.tl"),
        include_str!("../../../../apps/tl/notes/app.ids"),
    ),
    (
        "tasks",
        include_str!("../../../../apps/tl/tasks/app.tl"),
        include_str!("../../../../apps/tl/tasks/app.ids"),
    ),
];

/// The shell package's ids lock, as it ships.
#[must_use]
pub const fn shell_ids() -> &'static str {
    SHELL_IDS
}

/// The shell's layer over the desk: the `desk` view over the `Desk` root, the bar and the
/// surface under it. The shell package has two roots (`Desk` and its `Arrangement`), so each
/// layer names its own; the part is never "the first root's default view".
///
/// The part lays the whole desk view out at its band, so at rest only the bar's band shows and
/// a closed surface lays nothing out below it; while a surface or a menu of the bar's is up the
/// part covers the outputs and the same view has the room to show it.
#[must_use]
pub fn bar_layer() -> RootView {
    RootView {
        root: Some("Desk".to_owned()),
        view: Some("desk".to_owned()),
    }
}

/// The shell package and the packages it uses, compiled, the shell last: what a shell's instance
/// loads, in order. `None` when a package does not compile, which every build of the shell is
/// checked against, so a test that fails to compile here fails loudly.
///
/// # Errors
/// The packages' diagnostics, rendered with their file names.
pub fn compile_shell() -> Result<Vec<Compiled>, String> {
    let mut out: Vec<Compiled> = Vec::new();
    for (dir, source, ids) in SHELL_USES {
        let lock = tilolang_tilo::tilolang::IdsLock::parse(ids).map_err(|e| e.to_string())?;
        let imports: Vec<tilolang_tilo::tilolang::PackageMeta> =
            out.iter().map(|c| c.meta.clone()).collect();
        let compiled = tilolang_tilo::tilolang::compile_package(source, &lock, &imports).map_err(
            |diagnostics| {
                diagnostics
                    .iter()
                    .map(|d| d.render(&format!("apps/tl/{dir}/app.tl")))
                    .collect::<Vec<_>>()
                    .join("\n")
            },
        )?;
        out.push(compiled);
    }
    let lock = tilolang_tilo::tilolang::IdsLock::parse(SHELL_IDS).map_err(|e| e.to_string())?;
    let source = tilolang_tilo::tilolang::PackageSource::new(
        SHELL_FILES
            .iter()
            .map(|(path, text)| (format!("apps/tl/shell/{path}"), (*text).to_owned())),
    );
    let imports: Vec<tilolang_tilo::tilolang::PackageMeta> =
        out.iter().map(|c| c.meta.clone()).collect();
    let shell = tilolang_tilo::tilolang::compile_package_source(&source, &lock, &imports).map_err(
        |diagnostics| {
            diagnostics
                .iter()
                .map(|d| d.render(d.file.as_deref().unwrap_or("apps/tl/shell/app.tl")))
                .collect::<Vec<_>>()
                .join("\n")
        },
    )?;
    out.push(shell);
    Ok(out)
}
/// The capability that opens a running application's tray item, by its key: the desk's own
/// `open_tray_item` action answers it.
const CAP_TRAY: &str = "session.tray";

/// The bar's own `session.act` ids that are the desk's resource's, not the chrome's: the
/// question on the person's avatar is answered and put off where the desk reads it.
const DESK_ACTIONS: &[(&str, &str)] = &[
    ("answer_ask", DESK_RESOURCE),
    ("put_off_ask", DESK_RESOURCE),
];

/// How often the instance and its window may answer each other in one frame before the frame
/// is drawn as it is: a press, the action it ran, the change it committed, the effect settled.
const EXCHANGES: usize = 8;

/// A workspace as the desk's reading names it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceReading {
    pub key: String,
    pub name: String,
    pub current: bool,
}

/// A task running for the person, as the desk's reading names it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskReading {
    pub key: String,
    pub title: String,
    pub state: String,
}

/// Someone else here, as the desk's reading names them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonReading {
    pub key: String,
    pub name: String,
}

/// A window of the desk: a tab of a strip in the workspace in front, as the desk's reading
/// names it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowReading {
    pub key: String,
    pub title: String,
    pub origin_app: String,
    pub icon: String,
    pub strip: String,
    pub workspace: String,
    /// How recently it was in front: lower is more recent.
    pub recency: i64,
    pub focused: bool,
    /// Whether it may be pictured: `live`, `withheld` (it holds secrets) or `none`.
    pub preview: String,
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

/// A notification in the centre, as its sender or its task named it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoticeReading {
    pub key: String,
    pub title: String,
    pub body: String,
    pub origin_app: String,
    pub icon: String,
    /// `info`, `success`, `warning` or `error`.
    pub kind: String,
    pub read: bool,
    pub group: String,
    pub wants_reply: bool,
}

/// One action a notification offers, as its sender named it.
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
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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
    pub unread: u32,
    pub quiet: bool,
    pub status_panel: bool,
    pub workspace: String,
    pub strip: String,
    pub workspaces: Vec<WorkspaceReading>,
    pub tasks: Vec<TaskReading>,
    /// The surface the desk shows over its windows: `none`, `desk`, `overview`, `switcher`,
    /// `launcher`, `centre`, `workspaces` or `composer`.
    #[serde(default)]
    pub here: String,
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
    /// The notifications in the centre, newest first.
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
}

/// A workspace the person's desk shows, as their arrangement names it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrangementWorkspaceReading {
    pub key: String,
    pub name: String,
}

/// A row of the desk, as the person's arrangement names it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrangementRowReading {
    pub key: String,
    pub workspace: String,
    pub name: String,
    pub shell: String,
}

/// A strip of the desk, as the person's arrangement names it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrangementStripReading {
    pub key: String,
    pub workspace: String,
    pub row: String,
    /// Its width in per-mille of its row's usable width.
    pub width: i64,
    pub maximized: bool,
    pub active: String,
    pub placed: bool,
    pub canvas_x: i64,
    pub canvas_y: i64,
    pub canvas_width: i64,
    pub canvas_height: i64,
    pub shell: String,
}

/// A place of the desk, as the person's arrangement names it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrangementPlaceReading {
    pub key: String,
    pub strip: String,
    pub shows: String,
    pub shown_as: String,
    pub record: String,
    pub title: String,
    pub pinned: bool,
    pub shell: String,
}

/// The reading of [`ARRANGEMENT_RESOURCE`]: the person's arrangement as the session holds it,
/// as JSON on the door.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrangementReading {
    pub workspaces: Vec<ArrangementWorkspaceReading>,
    pub rows: Vec<ArrangementRowReading>,
    pub strips: Vec<ArrangementStripReading>,
    pub places: Vec<ArrangementPlaceReading>,
}

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

/// Which edges the shell's chrome stands on: what the part covers of the outputs. The session
/// reads it from the shell's record — a bar on the top or bottom, a dock on the bottom or the
/// top — and the part covers each band it names.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each edge's own facts, read one at a time"
)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HostEdges {
    /// The bar rides an edge (whether it shows now or not): the part covers its band.
    pub bar: bool,
    /// The bar rides the top rather than the bottom.
    pub bar_top: bool,
    /// The dock rides an edge.
    pub dock: bool,
    /// The dock rides the bottom rather than the top.
    pub dock_bottom: bool,
}

impl HostEdges {
    /// Whether the bar's band is at the top.
    #[must_use]
    pub const fn bar_at_top(&self) -> bool {
        self.bar && self.bar_top
    }

    /// Whether the dock's band is at the bottom.
    #[must_use]
    pub const fn dock_at_bottom(&self) -> bool {
        self.dock && self.dock_bottom
    }
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

/// Between the session and the shell's origin: the desk's reading the session last published,
/// what the shell asked of the door, and the door's answers. The session serves it every turn
/// of its loop ([`SessionLink::serve`]); the origin reads and writes the rest.
#[derive(Default)]
pub struct SessionLink {
    reading: RefCell<Option<DeskReading>>,
    /// A reading arrived the origin has not written yet.
    fresh: Cell<bool>,
    /// The person's arrangement the session last published, and whether the origin has taken
    /// it in.
    arrangement: RefCell<Option<ArrangementReading>>,
    arrangement_fresh: Cell<bool>,
    /// The readings of the resources the places name, by their session id: each with its type
    /// as the shell's runtime names another app's (`<app>#<Type>`) and its JSON.
    readings: RefCell<BTreeMap<String, (String, serde_json::Value)>>,
    /// A reading arrived the origin has not handed the runtime yet.
    readings_fresh: Cell<bool>,
    asked: RefCell<VecDeque<Ask>>,
    /// How many effects the shell handed the session, and how many the session ran.
    handed: Cell<u64>,
    served: Cell<u64>,
    answers: RefCell<BTreeMap<Ask, Result<(), String>>>,
}

impl SessionLink {
    /// The session published `reading`: the shell writes it on its next frame when it differs
    /// from the last. Whether it did.
    pub fn publish(&self, reading: DeskReading) -> bool {
        // What the desk reading names is the desk; what the screen says of its edges comes
        // with it, published by [`Self::publish_edges`], so the reading keeps the last word on
        // it: a desk change does not put the edges back to rest.
        let mut reading = reading;
        if let Some(was) = self.reading.borrow().as_ref() {
            reading.bar_reached = was.bar_reached;
            reading.bar_covered = was.bar_covered;
            reading.dock_reached = was.dock_reached;
            reading.dock_covered = was.dock_covered;
        }
        if self.reading.borrow().as_ref() == Some(&reading) {
            return false;
        }
        *self.reading.borrow_mut() = Some(reading);
        self.fresh.set(true);
        true
    }

    /// The session published `reading`, the person's arrangement as it stands now: the shell
    /// writes it on its next frame when it differs from the last. Whether it did.
    pub fn publish_arrangement(&self, reading: ArrangementReading) -> bool {
        if self.arrangement.borrow().as_ref() == Some(&reading) {
            return false;
        }
        *self.arrangement.borrow_mut() = Some(reading);
        self.arrangement_fresh.set(true);
        true
    }

    /// Run what the shell asked through `invoke` (the session's door), keeping each answer
    /// for the effect that asked. Whether anything ran.
    pub fn serve(&self, mut invoke: impl FnMut(&Ask) -> Result<(), String>) -> bool {
        let asked: Vec<Ask> = self.asked.borrow_mut().drain(..).collect();
        self.served
            .set(self.served.get().wrapping_add(asked.len() as u64));
        for ask in &asked {
            let answer = invoke(ask);
            if let Err(why) = &answer {
                tracing::warn!(?ask, why, "the session refused what the shell asked");
            }
            self.answers.borrow_mut().insert(ask.clone(), answer);
        }
        !asked.is_empty()
    }

    /// Whether the shell asked something the session has not run yet.
    #[must_use]
    pub fn has_asks(&self) -> bool {
        !self.asked.borrow().is_empty()
    }

    /// Publish what the screen says of its edges (`edges`), folded into the desk reading the
    /// session publishes: the pointer at an edge and a window standing where a bar or a dock
    /// there would. Published before the desk so a frame that moves both writes them once.
    pub fn publish_edges(&self, edges: ScreenEdges) {
        let Some(mut reading) = self.reading.borrow().clone() else {
            return;
        };
        if reading.bar_reached == edges.bar_reached
            && reading.bar_covered == edges.bar_covered
            && reading.dock_reached == edges.dock_reached
            && reading.dock_covered == edges.dock_covered
        {
            return;
        }
        reading.bar_reached = edges.bar_reached;
        reading.bar_covered = edges.bar_covered;
        reading.dock_reached = edges.dock_reached;
        reading.dock_covered = edges.dock_covered;
        // Written here, not through [`Self::publish`]: that keeps the edges the reading before
        // it held, which is what a desk change must not overwrite, and these are the edges
        // themselves.
        *self.reading.borrow_mut() = Some(reading);
        self.fresh.set(true);
    }

    /// The surface the desk shows now (`none` at rest): what the part covers the outputs for.
    #[must_use]
    pub fn surface(&self) -> String {
        self.reading
            .borrow()
            .as_ref()
            .map(|reading| reading.here.clone())
            .unwrap_or_default()
    }
}

/// The ask an outside effect of `capability` with `payload` is, or why it is none.
fn ask_of(capability: &str, payload: &Json) -> Result<Ask, String> {
    // `session.run` and `session.notify` carry a tuple; every other effect a text.
    if matches!(capability, CAP_RUN | CAP_NOTIFY) {
        return run_ask(payload, capability);
    }
    let Json::String(word) = payload else {
        return Err(format!("{capability} takes a text, not {payload}"));
    };
    match capability {
        CAP_SWITCH => Ok(Ask {
            resource: DESK_RESOURCE.to_owned(),
            action: SWITCH_WORKSPACE.to_owned(),
            arguments: BTreeMap::from([(WORKSPACE_ARG.to_owned(), word.clone())]),
        }),
        CAP_TRAY => Ok(Ask {
            resource: DESK_RESOURCE.to_owned(),
            action: OPEN_TRAY_ITEM.to_owned(),
            arguments: BTreeMap::from([(TRAY_KEY_ARG.to_owned(), word.clone())]),
        }),
        CAP_ACT => {
            // The desktop's own argument-less actions are the chrome's; the desk's few (the
            // question on the person's avatar) are the resource the bar reads it from.
            let resource = DESK_ACTIONS
                .iter()
                .find(|(id, _)| id == word)
                .map_or(CHROME_RESOURCE, |(_, resource)| resource);
            Ok(Ask {
                resource: (*resource).to_owned(),
                action: word.clone(),
                arguments: BTreeMap::new(),
            })
        }
        CAP_ARRANGE => {
            // `<action id> {json arguments}`, or just the id for an action that takes none.
            let (action, rest) = word
                .split_once(' ')
                .map_or_else(|| (word.trim(), ""), |(a, rest)| (a.trim(), rest.trim()));
            if action.is_empty() {
                return Err("session.arrange takes an action id".to_owned());
            }
            let given: BTreeMap<String, Json> = if rest.is_empty() {
                BTreeMap::new()
            } else {
                serde_json::from_str(rest).map_err(|error| format!("session.arrange: {error}"))?
            };
            let arguments = given
                .into_iter()
                .map(|(name, value)| {
                    let text = match value {
                        Json::String(text) => text,
                        other => other.to_string(),
                    };
                    (name, text)
                })
                .collect();
            Ok(Ask {
                resource: ARRANGEMENT_RESOURCE.to_owned(),
                action: action.to_owned(),
                arguments,
            })
        }
        other => Err(format!("the session takes no outside effect `{other}`")),
    }
}

/// The ask `session.run` carries: `(resource, action, (argument, value)…)`. The shell names the
/// session's own resource and action, as the door publishes them (a window's `focus` on
/// `desktop:tab:7`, the chrome's `move-tab-to-strip`), and each argument by the name the
/// action's own fields have. `session.notify` is the same, on the notification centre.
fn run_ask(payload: &Json, capability: &str) -> Result<Ask, String> {
    let Json::Array(items) = payload else {
        return Err(format!(
            "{capability} takes (resource, action, (argument, value)…), not {payload}"
        ));
    };
    let (Some(Json::String(resource)), Some(Json::String(action))) = (items.first(), items.get(1))
    else {
        return Err(format!("{capability} names its resource and action first"));
    };
    let mut arguments = BTreeMap::new();
    for argument in items.iter().skip(2) {
        let Json::Array(pair) = argument else {
            return Err(format!(
                "{capability} takes each argument as (name, value), not {argument}"
            ));
        };
        let (Some(Json::String(name)), Some(value)) = (pair.first(), pair.get(1)) else {
            return Err(format!(
                "{capability} takes each argument as (name, value), not {argument}"
            ));
        };
        arguments.insert(name.clone(), value_as_text(value));
    }
    Ok(Ask {
        resource: resource.clone(),
        action: action.clone(),
        arguments,
    })
}

/// What the origin last wrote of an item kept by key (a workspace, a task).
struct Kept<T> {
    at: ResourceRef,
    was: T,
}

/// The session as the origin of a shell's `session.*` resources. See the module docs.
pub struct SessionOrigin {
    link: Rc<SessionLink>,
    written: Option<DeskReading>,
    workspaces: BTreeMap<String, Kept<WorkspaceReading>>,
    tasks: BTreeMap<String, Kept<TaskReading>>,
    people: BTreeMap<String, Kept<PersonReading>>,
    windows: BTreeMap<String, Kept<WindowReading>>,
    agents: BTreeMap<String, Kept<AgentReading>>,
    rooms: BTreeMap<String, Kept<RoomReading>>,
    notices: BTreeMap<String, Kept<NoticeReading>>,
    notice_actions: BTreeMap<String, Kept<NoticeActionReading>>,
    arrangement_workspaces: BTreeMap<String, Kept<ArrangementWorkspaceReading>>,
    arrangement_rows: BTreeMap<String, Kept<ArrangementRowReading>>,
    arrangement_strips: BTreeMap<String, Kept<ArrangementStripReading>>,
    arrangement_places: BTreeMap<String, Kept<ArrangementPlaceReading>>,
    tray: BTreeMap<String, Kept<TrayReading>>,
    /// Effects handed to the session and not answered yet.
    in_flight: BTreeSet<Ask>,
}

impl SessionOrigin {
    #[must_use]
    pub fn new(link: Rc<SessionLink>) -> Self {
        Self {
            link,
            written: None,
            workspaces: BTreeMap::new(),
            tasks: BTreeMap::new(),
            people: BTreeMap::new(),
            windows: BTreeMap::new(),
            agents: BTreeMap::new(),
            rooms: BTreeMap::new(),
            notices: BTreeMap::new(),
            notice_actions: BTreeMap::new(),
            arrangement_workspaces: BTreeMap::new(),
            arrangement_rows: BTreeMap::new(),
            arrangement_strips: BTreeMap::new(),
            arrangement_places: BTreeMap::new(),
            tray: BTreeMap::new(),
            in_flight: BTreeSet::new(),
        }
    }

    fn answered(&self) -> bool {
        let answers = self.link.answers.borrow();
        self.in_flight.iter().any(|ask| answers.contains_key(ask))
    }

    /// The writes that bring the package's resources from what was written to `reading`.
    #[allow(
        clippy::too_many_lines,
        reason = "one pass over the desk's fields and its two collections"
    )]
    fn writes(
        &mut self,
        reading: &DeskReading,
        root: &ResourceRef,
        types: &[OriginType],
    ) -> Result<Vec<OriginWrite>, String> {
        let kind = |name: &str| {
            types
                .iter()
                .find(|ty| ty.kind == format!("{ORIGIN}.{name}"))
                .ok_or_else(|| format!("the shell declares no type of `{ORIGIN}.{name}`"))
        };
        let desk = kind("desk")?;
        let field = |ty: &OriginType, name: &str| ty.field(name).map(str::to_owned);
        let was = self.written.clone().unwrap_or_default();
        let mut fields = Vec::new();
        let mut set = |name: &str, now: Value, changed: bool| {
            if (changed || self.written.is_none())
                && let Some(id) = field(desk, name)
            {
                fields.push((id, now));
            }
        };
        set(
            "person",
            text(&reading.person),
            reading.person != was.person,
        );
        set("clock", text(&reading.clock), reading.clock != was.clock);
        set(
            "weekday",
            text(&reading.weekday),
            reading.weekday != was.weekday,
        );
        set("date", text(&reading.date), reading.date != was.date);
        set(
            "unread",
            Value::Int(i64::from(reading.unread)),
            reading.unread != was.unread,
        );
        set(
            "quiet",
            Value::Bool(reading.quiet),
            reading.quiet != was.quiet,
        );
        set(
            "status_panel",
            Value::Bool(reading.status_panel),
            reading.status_panel != was.status_panel,
        );
        set(
            "workspace",
            text(&reading.workspace),
            reading.workspace != was.workspace,
        );
        set("strip", text(&reading.strip), reading.strip != was.strip);
        // What the screen says of the edges a bar or dock that shows only sometimes follows.
        // They are the desk's, derived with the rest of it: the session makes them again every
        // start, so they are never the person's record and never need committing.
        set(
            "bar_reached",
            Value::Bool(reading.bar_reached),
            reading.bar_reached != was.bar_reached,
        );
        set(
            "bar_covered",
            Value::Bool(reading.bar_covered),
            reading.bar_covered != was.bar_covered,
        );
        set(
            "dock_reached",
            Value::Bool(reading.dock_reached),
            reading.dock_reached != was.dock_reached,
        );
        set(
            "dock_covered",
            Value::Bool(reading.dock_covered),
            reading.dock_covered != was.dock_covered,
        );
        set("here", text(&reading.here), reading.here != was.here);
        set(
            "spotlight",
            text(&reading.spotlight),
            reading.spotlight != was.spotlight,
        );
        set(
            "banner",
            text(&reading.banner),
            reading.banner != was.banner,
        );
        // The timer Clocks runs: its name and what it reads. Empty names mean no timer, so
        // the clock shows the time alone; paused and stopwatch say how to read it.
        let timer = reading.timer.clone().unwrap_or_default();
        set(
            "timer",
            text(&reading.timer_name()),
            reading.timer != was.timer,
        );
        set(
            "timer_left",
            text(&reading.timer_left()),
            reading.timer != was.timer,
        );
        set(
            "timer_paused",
            Value::Bool(timer.paused),
            reading.timer != was.timer,
        );
        set(
            "timer_stopwatch",
            Value::Bool(timer.stopwatch),
            reading.timer != was.timer,
        );
        // The ask on the person's avatar: its words, and whether it stands open.
        let ask = reading.ask.clone().unwrap_or_default();
        set("ask", text(&ask.question), reading.ask != was.ask);
        set("ask_detail", text(&ask.detail), reading.ask != was.ask);
        set("ask_answer", text(&ask.answer), reading.ask != was.ask);
        set("ask_open", Value::Bool(ask.open), reading.ask != was.ask);
        let mut writes = Vec::new();
        if !fields.is_empty() {
            writes.push(OriginWrite::Set {
                target: root.clone(),
                fields,
            });
        }
        // The running applications' tray items: each's key and the app's own words for it.
        let tray = reading.tray.clone();
        if let (Ok(ty), Some(collection)) = (kind("tray_item"), field(desk, "tray")) {
            writes.extend(sync_items(
                &mut self.tray,
                &tray,
                root,
                &collection,
                ty,
                |item| item.key.clone(),
                |item| {
                    vec![
                        ("key", text(&item.key)),
                        ("title", text(&item.title)),
                        ("menu", Value::Bool(item.menu)),
                    ]
                },
            ));
        }
        if let (Ok(ty), Some(collection)) = (kind("workspace"), field(desk, "workspaces")) {
            writes.extend(sync_items(
                &mut self.workspaces,
                &reading.workspaces,
                root,
                &collection,
                ty,
                |w| w.key.clone(),
                |w| {
                    vec![
                        ("key", text(&w.key)),
                        ("name", text(&w.name)),
                        ("current", Value::Bool(w.current)),
                    ]
                },
            ));
        }
        if let (Ok(ty), Some(collection)) = (kind("task"), field(desk, "tasks")) {
            writes.extend(sync_items(
                &mut self.tasks,
                &reading.tasks,
                root,
                &collection,
                ty,
                |t| t.key.clone(),
                |t| {
                    vec![
                        ("key", text(&t.key)),
                        ("title", text(&t.title)),
                        ("state", text(&t.state)),
                    ]
                },
            ));
        }
        if let (Ok(ty), Some(collection)) = (kind("person"), field(desk, "people")) {
            writes.extend(sync_items(
                &mut self.people,
                &reading.people,
                root,
                &collection,
                ty,
                |p| p.key.clone(),
                |p| vec![("key", text(&p.key)), ("name", text(&p.name))],
            ));
        }
        if let (Ok(ty), Some(collection)) = (kind("window"), field(desk, "windows")) {
            writes.extend(sync_items(
                &mut self.windows,
                &reading.windows,
                root,
                &collection,
                ty,
                |w| w.key.clone(),
                |w| {
                    vec![
                        ("key", text(&w.key)),
                        ("title", text(&w.title)),
                        ("origin_app", text(&w.origin_app)),
                        ("icon", text(&w.icon)),
                        ("strip", text(&w.strip)),
                        ("workspace", text(&w.workspace)),
                        ("recency", Value::Int(w.recency)),
                        ("focused", Value::Bool(w.focused)),
                        ("preview", text(&w.preview)),
                    ]
                },
            ));
        }
        if let (Ok(ty), Some(collection)) = (kind("agent"), field(desk, "agents")) {
            writes.extend(sync_items(
                &mut self.agents,
                &reading.agents,
                root,
                &collection,
                ty,
                |a| a.key.clone(),
                |a| {
                    vec![
                        ("key", text(&a.key)),
                        ("name", text(&a.name)),
                        ("state", text(&a.state)),
                        ("doing", text(&a.doing)),
                        ("mark", text(&a.mark)),
                        ("unseen", Value::Int(a.unseen)),
                    ]
                },
            ));
        }
        if let (Ok(ty), Some(collection)) = (kind("room"), field(desk, "rooms")) {
            writes.extend(sync_items(
                &mut self.rooms,
                &reading.rooms,
                root,
                &collection,
                ty,
                |r| r.key.clone(),
                |r| {
                    vec![
                        ("key", text(&r.key)),
                        ("name", text(&r.name)),
                        ("current", Value::Bool(r.current)),
                        ("members", text(&r.members)),
                        ("present", Value::Int(r.present)),
                        ("activity", text(&r.activity)),
                    ]
                },
            ));
        }
        if let (Ok(ty), Some(collection)) = (kind("notice"), field(desk, "notices")) {
            writes.extend(sync_items(
                &mut self.notices,
                &reading.notices,
                root,
                &collection,
                ty,
                |n| n.key.clone(),
                |n| {
                    vec![
                        ("key", text(&n.key)),
                        ("title", text(&n.title)),
                        ("body", text(&n.body)),
                        ("origin_app", text(&n.origin_app)),
                        ("icon", text(&n.icon)),
                        ("kind", text(&n.kind)),
                        ("read", Value::Bool(n.read)),
                        ("group", text(&n.group)),
                        ("wants_reply", Value::Bool(n.wants_reply)),
                    ]
                },
            ));
        }
        if let (Ok(ty), Some(collection)) = (kind("notice_action"), field(desk, "notice_actions")) {
            writes.extend(sync_items(
                &mut self.notice_actions,
                &reading.notice_actions,
                root,
                &collection,
                ty,
                |a| format!("{}:{}", a.notice, a.key),
                |a| {
                    vec![
                        ("notice", text(&a.notice)),
                        ("key", text(&a.key)),
                        ("label", text(&a.label)),
                    ]
                },
            ));
        }
        Ok(writes)
    }
}

/// The writes that bring the items of `collection` on `root` from `kept` to `now`, by key:
/// new ones inserted (at the place the writes return, filled in by the caller), gone ones
/// removed, changed ones set. Items keep the order they were inserted in.
fn sync_items<T: Clone + PartialEq>(
    kept: &mut BTreeMap<String, Kept<T>>,
    now: &[T],
    root: &ResourceRef,
    collection: &str,
    ty: &OriginType,
    key: impl Fn(&T) -> String,
    fields: impl Fn(&T) -> Vec<(&'static str, Value)>,
) -> Vec<OriginWrite> {
    let named = |item: &T| -> Vec<(String, Value)> {
        fields(item)
            .into_iter()
            .filter_map(|(name, value)| Some((ty.field(name)?.to_owned(), value)))
            .collect()
    };
    let mut writes = Vec::new();
    let keys: BTreeSet<String> = now.iter().map(&key).collect();
    kept.retain(|k, item| {
        let stays = keys.contains(k);
        if !stays {
            writes.push(OriginWrite::Remove {
                owner: root.clone(),
                field: collection.to_owned(),
                item: item.at.clone(),
            });
        }
        stays
    });
    for item in now {
        match kept.get_mut(&key(item)) {
            Some(was) if was.was == *item => {}
            Some(was) => {
                writes.push(OriginWrite::Set {
                    target: was.at.clone(),
                    fields: named(item),
                });
                was.was = item.clone();
            }
            None => writes.push(OriginWrite::Insert {
                owner: root.clone(),
                field: collection.to_owned(),
                ty: ty.ty.clone(),
                fields: named(item),
            }),
        }
    }
    writes
}

/// The field `name` as a text value.
fn text(s: &str) -> Value {
    Value::Text(s.to_owned())
}

/// The fields of an arrangement workspace, as it is written into the shell's record.
fn workspace_fields(w: &ArrangementWorkspaceReading) -> Vec<(&'static str, Value)> {
    vec![("key", text(&w.key)), ("name", text(&w.name))]
}

/// The fields of a row, as it is written into the shell's record.
fn row_fields(r: &ArrangementRowReading) -> Vec<(&'static str, Value)> {
    vec![
        ("key", text(&r.key)),
        ("workspace", text(&r.workspace)),
        ("name", text(&r.name)),
        ("shell", text(&r.shell)),
    ]
}

/// The fields of a strip, as it is written into the shell's record.
fn strip_fields(s: &ArrangementStripReading) -> Vec<(&'static str, Value)> {
    vec![
        ("key", text(&s.key)),
        ("workspace", text(&s.workspace)),
        ("row", text(&s.row)),
        ("width", Value::Int(s.width)),
        ("maximized", Value::Bool(s.maximized)),
        ("active", text(&s.active)),
        ("placed", Value::Bool(s.placed)),
        ("canvas_x", Value::Int(s.canvas_x)),
        ("canvas_y", Value::Int(s.canvas_y)),
        ("canvas_width", Value::Int(s.canvas_width)),
        ("canvas_height", Value::Int(s.canvas_height)),
        ("shell", text(&s.shell)),
    ]
}

/// The fields of a place, as it is written into the shell's record.
fn place_fields(p: &ArrangementPlaceReading) -> Vec<(&'static str, Value)> {
    vec![
        ("key", text(&p.key)),
        ("strip", text(&p.strip)),
        ("shows", text(&p.shows)),
        ("shown_as", text(&p.shown_as)),
        ("record", text(&p.record)),
        ("title", text(&p.title)),
        ("pinned", Value::Bool(p.pinned)),
        ("shell", text(&p.shell)),
    ]
}

impl SessionOrigin {
    /// Write the person's arrangement into the shell's `Arrangement` root, when the session
    /// published one the origin has not written yet. Whether anything was written.
    fn poll_arrangement(&mut self, cx: &OriginCx<'_>) -> bool {
        if !self.link.arrangement_fresh.replace(false) {
            return false;
        }
        let Some(reading) = self.link.arrangement.borrow().clone() else {
            return false;
        };
        let Some(types) = cx.host.find_in_packages(|package| {
            (package.meta.app == cx.app).then(|| origin_types(&package.meta, ORIGIN))
        }) else {
            tracing::error!(app = cx.app, "the shell's package is not loaded");
            return false;
        };
        // The root the `Arrangement` type is: `root-<type id>`, made the first time it is
        // wanted, as `root arrangement` reads it.
        let Some(kind) = types
            .iter()
            .find(|ty| ty.kind == format!("{ORIGIN}.arrangement"))
        else {
            // A shell without an arrangement view keeps its bar alone: nothing to write.
            return false;
        };
        let root = if cx.host.store().contains(&format!("root-{}", kind.ty)) {
            ResourceRef::root(format!("root-{}", kind.ty), kind.ty.clone())
        } else {
            match cx.host.runtime().create_root(&kind.ty) {
                Ok(root) => root,
                Err(error) => {
                    tracing::error!(%error, "the shell has no arrangement to write");
                    return false;
                }
            }
        };
        let (writes, inserted) = self.arrangement_writes(&reading, &root, &types);
        match cx
            .host
            .runtime()
            .apply_origin(ORIGIN, writes, Some(cx.now.clone()))
        {
            Ok(made) => {
                self.keep_inserted_arrangement(&reading, &types, inserted, made);
                true
            }
            Err(error) => {
                tracing::error!(%error, "the person's arrangement could not be written");
                false
            }
        }
    }

    /// The writes that bring the shell's arrangement root from what was written to `reading`,
    /// and the items they insert, by type and key.
    fn arrangement_writes(
        &mut self,
        reading: &ArrangementReading,
        root: &ResourceRef,
        types: &[OriginType],
    ) -> (Vec<OriginWrite>, Vec<(String, String)>) {
        let find = |name: &str| {
            types
                .iter()
                .find(|ty| ty.kind == format!("{ORIGIN}.{name}"))
        };
        let mut writes = Vec::new();
        let Some(ty) = find("arrangement") else {
            return (writes, Vec::new());
        };
        if let (Some(field), Some(item)) = (ty.field("workspaces"), find("arrangement_workspace")) {
            writes.extend(sync_items(
                &mut self.arrangement_workspaces,
                &reading.workspaces,
                root,
                field,
                item,
                |w| w.key.clone(),
                workspace_fields,
            ));
        }
        if let (Some(field), Some(item)) = (ty.field("rows"), find("arrangement_row")) {
            writes.extend(sync_items(
                &mut self.arrangement_rows,
                &reading.rows,
                root,
                field,
                item,
                |r| r.key.clone(),
                row_fields,
            ));
        }
        if let (Some(field), Some(item)) = (ty.field("strips"), find("arrangement_strip")) {
            writes.extend(sync_items(
                &mut self.arrangement_strips,
                &reading.strips,
                root,
                field,
                item,
                |s| s.key.clone(),
                strip_fields,
            ));
        }
        if let (Some(field), Some(item)) = (ty.field("places"), find("arrangement_place")) {
            writes.extend(sync_items(
                &mut self.arrangement_places,
                &reading.places,
                root,
                field,
                item,
                |p| p.key.clone(),
                place_fields,
            ));
        }
        // The keys of the items the writes insert, in their order: what the origin must find
        // where they were made.
        let inserted = writes
            .iter()
            .filter_map(|write| match write {
                OriginWrite::Insert { ty, fields, .. } => {
                    let key = types
                        .iter()
                        .find(|t| t.ty == *ty)
                        .and_then(|t| t.field("key"))
                        .and_then(|id| fields.iter().find(|(f, _)| f == id))
                        .and_then(|(_, v)| match v {
                            Value::Text(key) => Some(key.clone()),
                            _ => None,
                        });
                    Some((ty.clone(), key.unwrap_or_default()))
                }
                _ => None,
            })
            .collect();
        (writes, inserted)
    }

    /// Keep where the arrangement items a write inserted were made, by type and key.
    fn keep_inserted_arrangement(
        &mut self,
        reading: &ArrangementReading,
        types: &[OriginType],
        inserted: Vec<(String, String)>,
        made: Vec<Option<ResourceRef>>,
    ) {
        let mut made = made.into_iter().flatten();
        for (ty, key) in inserted {
            let Some(at) = made.next() else { break };
            let is = |name: &str| {
                types
                    .iter()
                    .any(|t| t.ty == ty && t.kind == format!("{ORIGIN}.{name}"))
            };
            if is("arrangement_workspace")
                && let Some(was) = reading.workspaces.iter().find(|w| w.key == key)
            {
                self.arrangement_workspaces.insert(
                    key,
                    Kept {
                        at,
                        was: was.clone(),
                    },
                );
            } else if is("arrangement_row")
                && let Some(was) = reading.rows.iter().find(|r| r.key == key)
            {
                self.arrangement_rows.insert(
                    key,
                    Kept {
                        at,
                        was: was.clone(),
                    },
                );
            } else if is("arrangement_strip")
                && let Some(was) = reading.strips.iter().find(|s| s.key == key)
            {
                self.arrangement_strips.insert(
                    key,
                    Kept {
                        at,
                        was: was.clone(),
                    },
                );
            } else if is("arrangement_place")
                && let Some(was) = reading.places.iter().find(|p| p.key == key)
            {
                self.arrangement_places.insert(
                    key,
                    Kept {
                        at,
                        was: was.clone(),
                    },
                );
            }
        }
    }
}

impl SessionOrigin {
    /// Keep where the items a write inserted (`inserted`, by type and key) were made
    /// (`made`, in the writes' order), so the next reading sets or removes them there.
    fn keep_inserted(
        &mut self,
        reading: &DeskReading,
        types: &[OriginType],
        inserted: Vec<(String, String)>,
        made: Vec<Option<ResourceRef>>,
    ) {
        let mut made = made.into_iter().flatten();
        for (ty, key) in inserted {
            let Some(at) = made.next() else { break };
            let is = |name: &str| {
                types
                    .iter()
                    .any(|t| t.ty == ty && t.kind == format!("{ORIGIN}.{name}"))
            };
            if is("workspace")
                && let Some(was) = reading.workspaces.iter().find(|w| w.key == key)
            {
                self.workspaces.insert(
                    key,
                    Kept {
                        at,
                        was: was.clone(),
                    },
                );
            } else if is("task")
                && let Some(was) = reading.tasks.iter().find(|t| t.key == key)
            {
                self.tasks.insert(
                    key,
                    Kept {
                        at,
                        was: was.clone(),
                    },
                );
            } else if is("person")
                && let Some(was) = reading.people.iter().find(|p| p.key == key)
            {
                self.people.insert(
                    key,
                    Kept {
                        at,
                        was: was.clone(),
                    },
                );
            } else if is("window")
                && let Some(was) = reading.windows.iter().find(|w| w.key == key)
            {
                self.windows.insert(
                    key,
                    Kept {
                        at,
                        was: was.clone(),
                    },
                );
            } else if is("agent")
                && let Some(was) = reading.agents.iter().find(|a| a.key == key)
            {
                self.agents.insert(
                    key,
                    Kept {
                        at,
                        was: was.clone(),
                    },
                );
            } else if is("room")
                && let Some(was) = reading.rooms.iter().find(|r| r.key == key)
            {
                self.rooms.insert(
                    key,
                    Kept {
                        at,
                        was: was.clone(),
                    },
                );
            } else if is("notice")
                && let Some(was) = reading.notices.iter().find(|n| n.key == key)
            {
                self.notices.insert(
                    key,
                    Kept {
                        at,
                        was: was.clone(),
                    },
                );
            } else if is("notice_action")
                && let Some(was) = reading
                    .notice_actions
                    .iter()
                    .find(|a| format!("{}:{}", a.notice, a.key) == key)
            {
                self.notice_actions.insert(
                    key,
                    Kept {
                        at,
                        was: was.clone(),
                    },
                );
            }
        }
    }
}

/// `value` as the door's arguments take it: text as text, a number or a flag as its text.
fn value_as_text(value: &Json) -> String {
    match value {
        Json::String(text) => text.clone(),
        other => other.to_string(),
    }
}

impl Origin for SessionOrigin {
    fn name(&self) -> &str {
        ORIGIN
    }

    fn serves(&self, capability: &str) -> bool {
        capability
            .split_once('.')
            .is_some_and(|(origin, _)| origin == ORIGIN)
    }

    fn due(&self) -> bool {
        self.link.fresh.get()
            || self.link.arrangement_fresh.get()
            || self.link.readings_fresh.get()
            || self.answered()
    }

    fn poll(&mut self, cx: &OriginCx<'_>) -> Polled {
        let mut polled = Polled {
            wrote: false,
            settle: self.answered(),
        };
        let wrote_arrangement = self.poll_arrangement(cx);
        polled.wrote |= wrote_arrangement;
        polled.wrote |= self.poll_readings(cx);
        if !self.link.fresh.replace(false) {
            return polled;
        }
        let Some(reading) = self.link.reading.borrow().clone() else {
            return polled;
        };
        let Some(types) = cx.host.find_in_packages(|package| {
            (package.meta.app == cx.app).then(|| origin_types(&package.meta, ORIGIN))
        }) else {
            tracing::error!(app = cx.app, "the shell's package is not loaded");
            return polled;
        };
        let root = match cx.host.root_of_layer(cx.app, &bar_layer()) {
            Ok(root) => root,
            Err(error) => {
                tracing::error!(%error, "the shell has no desk to write the session into");
                return polled;
            }
        };
        let writes = match self.writes(&reading, &root, &types) {
            // The desk's own change is applied below, in the origin's own transaction.
            Ok(writes) => writes,
            Err(why) => {
                tracing::error!(why, "the session's desk does not fit the shell");
                return polled;
            }
        };
        let inserted: Vec<(String, String)> = writes
            .iter()
            .filter_map(|write| match write {
                OriginWrite::Insert { ty, fields, .. } => {
                    let key = types
                        .iter()
                        .find(|t| t.ty == *ty)
                        .and_then(|t| t.field("key"))
                        .and_then(|id| fields.iter().find(|(f, _)| f == id))
                        .and_then(|(_, v)| match v {
                            Value::Text(key) => Some(key.clone()),
                            _ => None,
                        })?;
                    Some((ty.clone(), key))
                }
                _ => None,
            })
            .collect();
        match cx
            .host
            .runtime()
            .apply_origin(ORIGIN, writes, Some(cx.now.clone()))
        {
            Ok(made) => {
                self.keep_inserted(&reading, &types, inserted, made);
                self.written = Some(reading);
                polled.wrote = true;
            }
            Err(error) => {
                // Nothing was written: the next reading tries again from what was.
                tracing::error!(%error, "the session's desk could not be written");
            }
        }
        polled
    }

    fn deliver(&mut self, capability: &str, payload: &Json) -> Delivery {
        let ask = match ask_of(capability, payload) {
            Ok(ask) => ask,
            Err(why) => return Delivery::Rejected(why),
        };
        if let Some(answer) = self.link.answers.borrow_mut().remove(&ask) {
            self.in_flight.remove(&ask);
            return match answer {
                Ok(()) => Delivery::Delivered,
                Err(why) => Delivery::Rejected(why),
            };
        }
        if self.in_flight.insert(ask.clone()) {
            self.link.asked.borrow_mut().push_back(ask);
            self.link.handed.set(self.link.handed.get().wrapping_add(1));
        }
        // Answered by the session's next turn ([`SessionLink::serve`]), which the poll that
        // takes the answer in settles.
        Delivery::Pending
    }
}

/// What a shell's instance asks of the session's process.
struct ShellPlatform {
    packages: Rc<Vec<Compiled>>,
    waker: Waker,
    /// Where what the composed views change of other apps' resources goes.
    link: Rc<SessionLink>,
}

impl Platform for ShellPlatform {
    fn load(&self, host: &Host) -> u64 {
        for compiled in self.packages.iter() {
            if let Err(error) = host.load_compiled(compiled) {
                tracing::error!(app = compiled.meta.app, %error, "a shell package did not load");
            }
        }
        host.runtime().set_invoker(PlaceInvoker {
            link: Rc::clone(&self.link),
        });
        0
    }

    fn phrase(&self, id: &str, args: &[(String, String)]) -> String {
        let args: Vec<(&str, ui::i18n::ArgValue)> = args
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str().into()))
            .collect();
        tilo_ui_host::i18n::t_args(id, &args)
    }

    fn now(&self) -> String {
        ui::clock::local_now()
            .with_timezone(&chrono::Utc)
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string()
    }

    fn zone(&self) -> i64 {
        i64::from(ui::clock::local_now().offset().local_minus_utc())
    }

    fn waker(&self) -> Waker {
        std::sync::Arc::clone(&self.waker)
    }
}

/// The person at the device, as the shell names them.
struct Person(String);

impl NameHost for Person {
    fn name(&self, who: &Principal) -> String {
        if who.0.is_empty() {
            self.0.clone()
        } else {
            who.0.clone()
        }
    }
}

/// The room a shell's record is decided in: the person's own, on this device.
const SHELL_ROOM: &str = "shell";

/// `person`'s standing over their own shell `app`: they own its record, and their shell may
/// hand the session its outside effects (`session.*`) without asking each time, as their
/// chosen shell. The grant is theirs, in the one policy every door decides with.
fn shell_standing(
    app: &str,
    person: &str,
) -> impl Fn() -> Option<tilo_action_policy::Standing> + 'static {
    use tilo_action_policy::{
        Answer, Grant, KindSet, Standing, Subject, TreePath, WorkspacePolicy,
    };
    let mut policy = WorkspacePolicy::new(SHELL_ROOM, person);
    policy.grants.push(Grant::new(
        format!("shell:{app}:session"),
        Subject::app(app),
        TreePath::room(SHELL_ROOM).app(app),
        KindSet::OUTSIDE,
        Answer::Allow,
        Subject::person(person),
    ));
    let standing = Standing::person(policy, person);
    move || Some(standing.clone())
}

/// How much work the part did: what its idle and its frames cost.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ShellWork {
    /// Frames in which the instance ran (something had changed).
    pub turns: u64,
    /// Frames the toolkit drew and the CPU rasterized.
    pub drawn: u64,
    /// Time the instance and the window spent answering each other (the view run, its
    /// patches, the widgets reconciled) and the layout after it.
    pub turning: std::time::Duration,
    /// Time the toolkit spent drawing those frames and the CPU rasterizing them.
    pub drawing: std::time::Duration,
    /// Of [`Self::turning`]: the instance's updates (views run, patches made), the commits
    /// to the shell's record, the window taking the patches in, and the layout after.
    pub updating: std::time::Duration,
    pub committing: std::time::Duration,
    pub receiving: std::time::Duration,
    pub laying_out: std::time::Duration,
    /// Of [`Self::committing`]: the instance reading the record anew after a commit.
    pub syncing: std::time::Duration,
    /// Effects the shell handed the session's door, and those the session ran.
    pub handed: u64,
    pub served: u64,
}

/// One part of a tilolang shell on the desktop. See the module docs.
pub struct ShellPart {
    widgets: WidgetHost,
    instance: Instance,
    /// The shell's record, shared by every part of one shell: the bar draws it and each other
    /// display draws the same, so a change any part commits is the whole shell's.
    record: Rc<RefCell<tilo_resource::Doc>>,
    origin: Rc<RefCell<SessionOrigin>>,
    /// Input reached the part since its last turn.
    input: bool,
    /// The last turn changed something: the next looks again, for what that started.
    again: bool,
    work: ShellWork,
    /// What the shell refused, in its words.
    refusals: Vec<String>,
    /// How high the part stands at rest (the bar's band), logical pixels.
    band: u32,
    /// The output this part stands on: its connector name, and its logical rectangle
    /// `(x, y, w, h)`. `None` before the outputs are told, when it spans their bounding box.
    output: Option<(String, (i32, i32, u32, u32))>,
    /// The display the part stood on, which went (unplugged, turned off): while it is gone the
    /// part stands nowhere, draws nothing, takes no pointer and offers nothing on the door,
    /// until it is told a display again.
    gone: Option<String>,
    /// The outputs a fullscreen window covers, by connector name: the part on one of them
    /// steps aside, as the desktop's own bar does there, and leaves the display to the window.
    fullscreen: Vec<String>,
    /// A menu the part opened, or a surface of the desk's (its overview, the switcher, the
    /// launcher, the notifications), is up: the part covers the outputs so it has room. At
    /// rest it covers only its bands, so a change draws only that.
    cover: bool,
    /// The edges the shell's chrome stands on: what the part covers of the outputs.
    edges: HostEdges,
    /// The shell's record's heads the instance was last brought up to date with: a commit any
    /// part of the shell made reaches this one by them, so every display shows one shell.
    seen_heads: Vec<String>,
}

/// A shell part's identity: the package it runs, the person it is for, and the record the
/// shell's parts share.
pub struct ShellIdentity<'a> {
    /// The app id of the compiled shell package.
    pub app: &'a str,
    /// The packages the instance runs, the shell last.
    pub packages: Vec<Compiled>,
    /// The person the shell is arranged by.
    pub person: &'a str,
    /// The link the session publishes its desk on and the shell asks through.
    pub link: Rc<SessionLink>,
    /// The shell's record, shared by every part of the shell.
    pub record: Rc<RefCell<tilo_resource::Doc>>,
    /// The first part of a shell makes its record and seeds it; every other part finds it
    /// made, so one shell has one record and one seed, however many displays draw it.
    pub seed: bool,
}

impl ShellPart {
    /// The root view of `identity.app` (the last of its packages, which come in load order) for
    /// its person, across the top `band` logical pixels of the part's output at `scale`, kept in
    /// step with the session through its link; `waker` wakes the session's loop when work the
    /// instance waits for is ready.
    ///
    /// The views' wells ([`ui::widget::Wells`]) report to `wells`, the handle the window policy
    /// placing the windows they stand for shares (`crate::WellPolicy`); `None` for a part with
    /// no windows of its own (the bar).
    #[must_use]
    pub fn new(
        identity: ShellIdentity<'_>,
        band: u32,
        scale: f64,
        waker: Waker,
        wells: Option<ui::widget::Wells>,
    ) -> Self {
        let ShellIdentity {
            app,
            packages,
            person,
            link,
            record,
            seed,
        } = identity;
        let placement = Placement {
            origin: (0, 0),
            size: (1, band.max(1)),
            scale,
        };
        tilo_ui_host::i18n::ensure();
        let (app_end, window_end) = pair();
        let mut instance = Instance::new(
            Config {
                app: app.to_owned(),
                me: person.to_owned(),
                mode: Mode::Live,
                device: None,
                index_dir: None,
                record_loader: None,
                sink: None,
                // The bar's band shows the bar, not whatever root the package lists first.
                root: bar_layer(),
            },
            Rc::new(ShellPlatform {
                packages: Rc::new(packages),
                waker,
                link: Rc::clone(&link),
            }),
            Rc::new(Person(person.to_owned())),
            Box::new(app_end),
        );
        instance.set_standing(Rc::new(shell_standing(app, person)));
        let origin = Rc::new(RefCell::new(SessionOrigin::new(link)));
        instance.add_origin(Rc::clone(&origin) as tilolang_tilo::origin::SharedOrigin);
        {
            let record = record.borrow();
            instance.sync(Some(&record));
        }
        let window = AppWindow::part(Box::new(window_end));
        let mut widgets = WidgetHost::new(Box::new(window), placement, false);
        widgets.install_wells(wells);
        let mut part = Self {
            widgets,
            instance,
            record,
            origin,
            input: false,
            again: true,
            work: ShellWork::default(),
            refusals: Vec::new(),
            band: band.max(1),
            output: None,
            gone: None,
            fullscreen: Vec::new(),
            cover: false,
            edges: HostEdges {
                bar: true,
                bar_top: true,
                dock: false,
                dock_bottom: false,
            },
            seen_heads: Vec::new(),
        };
        if seed {
            part.seed();
        }
        part.turn();
        part
    }

    /// Make the shell's own record (`root shell: Shell`) and let the package's migration seed
    /// it — the bar's parts and the dock's items, on a shell never arranged — as the package
    /// version's change, as an app's migration is. Both go through the part's own turns: the
    /// record is created as a queued change, the migration runs once the record is had, and
    /// [`Self::commit`] writes each into the part's record.
    fn seed(&mut self) {
        let Some(host) = self.instance.host() else {
            return;
        };
        let roots = host
            .package(self.instance.app())
            .map(|package| package.meta.named_roots)
            .unwrap_or_default();
        for root in &roots {
            if let Err(error) = host.root_record(self.instance.app(), &root.ty) {
                tracing::warn!(%error, "the shell's record could not be made");
                return;
            }
        }
        if roots.is_empty() {
            return;
        }
        // The package's migrations run for the room's origin alone, as an app's do. Each
        // committed action is written into the part's record, as the door writes an app's: the
        // record's making first, then the seed that runs once the record is had.
        self.instance.set_origin(true);
        for _ in 0..3 {
            {
                let record = self.record.borrow();
                self.instance.sync(Some(&record));
            }
            self.instance.commit_planned();
            if !self.commit() {
                break;
            }
        }
        // The seed moved the record: the part's next turn settles what it started (a view
        // that read a bound field runs again), as any change of the record does.
        self.instance.moved();
        // What the seed wrote is the shell's for every part: the heads here are the ones a
        // part made later finds and takes in.
        self.seen_heads = self
            .record
            .borrow()
            .committed_heads()
            .iter()
            .map(ToString::to_string)
            .collect();
    }

    /// Stand on this part's output: the whole of it while a menu of the part's is up, and else
    /// the band at each edge the shell's bar and dock ride — a bar at the top takes the top
    /// band, a dock at the bottom the bottom one — through `edges`. A shell whose chrome is off
    /// at an edge, or is a bar at the top alone (the first shell), takes that band only.
    ///
    /// A part on an output a fullscreen window covers takes no room and draws nothing: the
    /// window owns the display there, as it owns the desktop's own bar's display.
    fn place(&mut self, edges: HostEdges) {
        let Some((name, (x, y, width, output_height))) = self.output.clone() else {
            return;
        };
        let scale = self.widgets.placement().scale;
        if self.fullscreen.contains(&name) && !self.cover {
            // Nothing of the part stands there: the smallest placement keeps its layout cheap,
            // and `hidden` hands the compositor no layer and takes no pointer, so not a row of
            // it lies over the window.
            self.widgets.set_placement(Placement {
                origin: (x, y),
                size: (width.max(1), 1),
                scale,
            });
            self.widgets.relayout();
            return;
        }
        let band = self.band.min(output_height);
        // A menu of the part's, or a surface of the desk's, takes the whole output; else the
        // part stands in the band at each edge the chrome rides, so a change draws only what
        // shows there.
        let at_top = !self.cover && edges.bar_at_top();
        let at_bottom = !self.cover && edges.dock_at_bottom();
        let height = match (at_top, at_bottom) {
            (true, false) | (false, true) => band,
            _ => output_height.max(band),
        };
        let origin_y = if at_bottom && !at_top {
            y.saturating_add(
                i32::try_from(output_height.saturating_sub(height)).unwrap_or(i32::MAX),
            )
        } else {
            y
        };
        self.widgets.set_placement(Placement {
            origin: (x, origin_y),
            size: (width.max(1), height.max(1)),
            scale,
        });
        // A view laid out at the band keeps its old height until it is laid out again: the
        // menus and overlays it only holds room for when it covers the outputs need that.
        self.widgets.relayout();
    }

    /// Follow whether the part stands over the whole outputs: while a menu it opened is up,
    /// or while a surface of the desk's is (the overview, the switcher, the launcher, the
    /// notifications, the workspaces, the composer). At rest it goes back to the band, so a
    /// change draws only that.
    fn follow_cover(&mut self) {
        let menu = self
            .widgets
            .host_mut()
            .a11y_nodes()
            .iter()
            .any(|(_, node)| node.role() == ui::accesskit::Role::Menu);
        let surface = !matches!(
            self.origin.borrow().link.surface().as_str(),
            "" | "none" | "desk"
        );
        let cover = menu || surface;
        if cover != self.cover {
            self.cover = cover;
            self.place(self.edges);
        }
    }

    /// How much work the part did since it started.
    #[must_use]
    pub fn work(&self) -> ShellWork {
        let origin = self.origin.borrow();
        ShellWork {
            handed: origin.link.handed.get(),
            served: origin.link.served.get(),
            ..self.work
        }
    }

    /// Where the control assistive technology names `name` stands, in the desktop's logical
    /// layout `(x, y, width, height)`: what a test or a tool presses it by.
    pub fn control_bounds(&mut self, name: &str) -> Option<(f64, f64, f64, f64)> {
        let (ox, oy) = self.widgets.placement().origin;
        self.widgets
            .host_mut()
            .a11y_nodes()
            .into_iter()
            .filter(|(_, node)| node.label() == Some(name))
            .find_map(|(_, node)| node.bounds())
            .map(|rect| {
                (
                    f64::from(ox) + rect.x0,
                    f64::from(oy) + rect.y0,
                    rect.width(),
                    rect.height(),
                )
            })
    }

    /// The edges the shell's chrome stands on, as the session read them from the shell's
    /// record: where the part covers the outputs. A change moves the part to the edges it
    /// now names.
    pub fn set_edges(&mut self, edges: HostEdges) {
        if edges == self.edges {
            return;
        }
        self.edges = edges;
        self.place(edges);
    }

    /// Stand the part on `output`, its connector name and logical rectangle `(x, y, w, h)`:
    /// the part is one display's, and follows that display as it is added, moved or turned
    /// off. Every part of one shell is told its own display; none spans them all. `None`: the
    /// display went, and the part stands nowhere until it is told one again.
    pub fn set_display(&mut self, output: Option<(String, (i32, i32, u32, u32))>) {
        if self.output == output {
            return;
        }
        if output.is_some() {
            self.gone = None;
        } else {
            // What it drew goes with its display: nothing of it stands anywhere now.
            self.gone = self.output.take().map(|(name, _)| name);
            self.cover = false;
        }
        self.output = output;
        self.place(self.edges);
    }

    /// Whether the display the part stood on went, and it stands nowhere now.
    fn retired(&self) -> bool {
        self.gone.is_some()
    }

    /// Whether the part shows nothing at all now: its display went, or a fullscreen window
    /// owns it. It has no layers and takes no pointer then.
    fn hidden(&self) -> bool {
        self.retired() || self.aside()
    }

    /// Tell the part which output's or outputs' windows cover the whole display, by connector
    /// name: the part on one of them steps aside and draws nothing, as the desktop's own bar
    /// does where a window owns its display. A change steps it back when they go.
    pub fn set_fullscreen(&mut self, outputs: Vec<String>) {
        if self.fullscreen == outputs {
            return;
        }
        self.fullscreen = outputs;
        self.place(self.edges);
    }

    /// Where the part stands now, in the desktop's logical layout `(x, y, w, h)`, and the
    /// display it names: `None` until it is told of its output.
    #[must_use]
    pub fn placement(&self) -> Option<(String, (i32, i32, u32, u32))> {
        self.output.clone()
    }

    /// Whether the part draws nothing now: it stands on a display a fullscreen window owns.
    #[must_use]
    pub fn aside(&self) -> bool {
        self.output.as_ref().is_some_and(|(name, _)| {
            !self.cover && self.fullscreen.iter().any(|covered| covered == name)
        })
    }

    /// The shell's record as shared by every part of this shell: a part that commits a change
    /// writes it here, and the rest read it on their next turn.
    #[must_use]
    pub fn record_handle(&self) -> Rc<RefCell<tilo_resource::Doc>> {
        Rc::clone(&self.record)
    }

    /// The shell's own record (`root shell: Shell`) as it holds it now, when the package has
    /// one and the record is made: what its edge settings are read from.
    #[must_use]
    pub fn shell_record(&self) -> Option<tilolang_tilo::record::DynRecord> {
        let host = self.instance.host()?;
        let package = host.package(self.instance.app())?;
        let root = package
            .meta
            .named_roots
            .iter()
            .find(|root| root.name == SHELL_RECORD)?;
        host.store().record(&format!("root-{}", root.ty)).ok()
    }

    /// The package the part shows, as it was compiled: its types and their fields.
    #[must_use]
    pub fn package(&self) -> Option<tilolang_tilo::host::Package> {
        self.instance.host()?.package(self.instance.app())
    }

    /// The toolkit's host of the part's widget tree, to drive it directly.
    pub fn widgets_mut(&mut self) -> &mut WidgetHost {
        &mut self.widgets
    }

    fn window(&mut self) -> Option<&mut AppWindow> {
        self.widgets
            .host_mut()
            .root_widget_mut()
            .as_any_mut()?
            .downcast_mut::<AppWindow>()
    }

    /// Commit what the instance planned to the shell's record, as the door would, and bring
    /// the instance up to date with it. Whether anything was committed.
    fn commit(&mut self) -> bool {
        let mut committed = false;
        while let Some((invocation, _)) = self.instance.take_invocation() {
            let address = invocation.address.clone();
            if address.resource != HOST_RESOURCE || address.action != COMMIT {
                self.instance.answered(
                    &address,
                    Some("a shell part runs only its own commits".to_owned()),
                );
                continue;
            }
            let ticket = invocation
                .arguments
                .get("ticket")
                .and_then(Json::as_u64)
                .unwrap_or_default();
            let applied = self
                .instance
                .host()
                .and_then(|host| host.store().take_ticket(ticket))
                .ok_or_else(|| format!("no committed action {ticket}"))
                .and_then(|action| {
                    action
                        .apply(&mut self.record.borrow_mut())
                        .map_err(|error| error.to_string())
                });
            if let Err(why) = &applied {
                tracing::error!(why, "a shell change could not be committed");
            }
            self.instance.answered(&address, applied.err());
            committed = true;
        }
        if committed {
            // The record holds the changes once they are committed: the instance reads them
            // from its heads, and every view runs again, since the change is the record's.
            {
                let mut record = self.record.borrow_mut();
                record.commit();
                self.seen_heads = record
                    .committed_heads()
                    .iter()
                    .map(ToString::to_string)
                    .collect();
            }
            let at = std::time::Instant::now();
            let record = self.record.borrow();
            self.instance.sync(Some(&record));
            self.work.syncing = self.work.syncing.saturating_add(at.elapsed());
        }
        committed
    }

    /// Let the window and the instance answer each other until neither has more to say.
    /// Whether the window took anything.
    fn turn(&mut self) -> bool {
        let started = std::time::Instant::now();
        self.work.turns = self.work.turns.wrapping_add(1);
        let mut shown = false;
        let mut quiet = false;
        let lap = |since: &mut std::time::Instant| {
            let now = std::time::Instant::now();
            let took = now.saturating_duration_since(*since);
            *since = now;
            took
        };
        for _ in 0..EXCHANGES {
            let mut at = std::time::Instant::now();
            if let Some(window) = self.window() {
                window.flush();
            }
            let updated = self.instance.update();
            self.work.updating = self.work.updating.saturating_add(lap(&mut at));
            let committed = self.commit();
            self.work.committing = self.work.committing.saturating_add(lap(&mut at));
            let received = self.window().is_some_and(AppWindow::receive);
            self.hear_notices();
            self.work.receiving = self.work.receiving.saturating_add(lap(&mut at));
            shown |= received;
            if !(updated || committed || received) {
                quiet = true;
                break;
            }
        }
        if shown {
            let at = std::time::Instant::now();
            self.widgets.relayout();
            self.work.laying_out = self.work.laying_out.saturating_add(at.elapsed());
        }
        // A change another display's part of this shell committed reaches this one: the record
        // it shares moved on from the heads this instance saw, and it draws the shell as it is
        // now. Nothing else changed, so no other frame is owed.
        let mut record_moved = false;
        {
            let heads: Vec<String> = self
                .record
                .borrow()
                .committed_heads()
                .iter()
                .map(ToString::to_string)
                .collect();
            if heads != self.seen_heads {
                self.seen_heads = heads;
                let record = self.record.borrow();
                self.instance.sync(Some(&record));
                self.instance.moved();
                record_moved = true;
            }
        }
        if record_moved {
            // The views that read a bound field run again in this turn, so the part draws the
            // shell's change now rather than a frame late.
            let mut at = std::time::Instant::now();
            self.instance.update();
            self.work.updating = self.work.updating.saturating_add(at.elapsed());
            at = std::time::Instant::now();
            shown |= self.window().is_some_and(AppWindow::receive);
            self.work.receiving = self.work.receiving.saturating_add(at.elapsed());
            if shown {
                self.widgets.relayout();
                self.work.laying_out = self.work.laying_out.saturating_add(at.elapsed());
            }
        }
        // The exchanges ran until neither side had more to say, unless they ran out first:
        // then the next frame goes on.
        self.again = !quiet;
        self.work.turning = self.work.turning.saturating_add(started.elapsed());
        shown
    }

    /// What the shell's views said that a part has no window to show: a refusal is the
    /// session's to hear and is kept for whoever asks ([`Self::refusals`]).
    fn hear_notices(&mut self) {
        let Some(window) = self.window() else {
            return;
        };
        for (kind, text) in window.take_notices() {
            match kind {
                tilo_render_protocol::patch::NoticeKind::Error => {
                    tracing::warn!(text, "the shell refused what was asked of it");
                    self.refusals.push(text);
                }
                tilo_render_protocol::patch::NoticeKind::Info => {
                    tracing::info!(text, "the shell says");
                }
            }
        }
    }

    /// What the shell refused since it started, in its own words.
    #[must_use]
    pub fn refusals(&self) -> &[String] {
        &self.refusals
    }

    fn due(&self) -> bool {
        self.input || self.again || self.origin.borrow().due()
    }
}

impl crate::Published for ShellPart {
    /// A part whose display went offers nothing: the parts still standing answer the door.
    fn offered(&self) -> bool {
        !self.retired()
    }

    fn resources(&mut self) -> Vec<ui::tilo_actions::ResourceSpec> {
        crate::Published::resources(&mut self.widgets)
    }

    fn revision(&self) -> Option<u64> {
        crate::Published::revision(&self.widgets)
    }

    fn invoke(
        &mut self,
        address: &ui::tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ui::host_operation::ResourceOperationStart, ui::tilo_actions::DoorError> {
        self.input = true;
        crate::Published::invoke(&mut self.widgets, address, arguments)
    }

    fn accessible_labels(&mut self) -> Vec<String> {
        crate::Published::accessible_labels(&mut self.widgets)
    }
}

/// The keysym of Tab, as the layout reports it: Alt+Tab and Alt+Shift+Tab walk the switcher.
const XKB_TAB: u32 = 0xff09;
/// The keysym of `Alt_L`.
const XKB_ALT_L: u32 = 0xffe9;

/// What a press means to the shell's surfaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SurfaceKey {
    /// Alt+Tab: open the switcher on the window before the one in front, walking on from
    /// there (back with Shift).
    Walk(bool),
    /// Enter, or letting Alt go while the switcher is up: go to the window it reached.
    Commit,
    /// Esc: close the surface that is up.
    Close,
}

impl ShellPart {
    /// What `key` means to the shell while a surface of the desk's is up, or as a press that
    /// opens one. `None` when it belongs to whatever has the keyboard.
    fn surface_key(&self, key: &HostKey) -> Option<SurfaceKey> {
        if key.keysym == XKB_TAB && key.modifiers.alt && key.pressed {
            return Some(SurfaceKey::Walk(key.modifiers.shift));
        }
        let up = self.origin.borrow().link.surface();
        if matches!(up.as_str(), "" | "none" | "desk") {
            return None;
        }
        if up == "switcher" && matches!(key.keysym, XKB_ALT_L | 0xffea) && !key.pressed {
            return Some(SurfaceKey::Commit);
        }
        match key.keysym {
            0xff0d if key.pressed && up == "switcher" => Some(SurfaceKey::Commit),
            0xff1b if key.pressed => Some(SurfaceKey::Close),
            _ => None,
        }
    }
}

impl SurfaceHost for ShellPart {
    fn layers(&self) -> Vec<HostLayer> {
        // Aside for a fullscreen window, the window has the whole display: not a row of the
        // bar's pixels stands over it.
        if self.hidden() {
            return Vec::new();
        }
        self.widgets.layers()
    }

    fn claim_key(&mut self, key: &HostKey, locked: bool, _inhibited: bool) -> KeyClaim {
        // A shell's overlays answer the keyboard: Alt+Tab walks the switcher, Enter goes there,
        // and Esc closes whatever surface is up. A locked session is not the shell's to answer.
        if locked || self.surface_key(key).is_none() {
            return KeyClaim::Pass;
        }
        self.input = true;
        KeyClaim::Take
    }

    fn accepts_pointer(&self, x: f64, y: f64) -> bool {
        if self.hidden() {
            return false;
        }
        // While a menu of the part's is up, a press anywhere over it is the part's: outside
        // the menu it closes it, as a press outside any menu does.
        if self.cover
            && let Some((_, (ox, oy, width, height))) = self.output
        {
            let (x, y) = (x - f64::from(ox), y - f64::from(oy));
            return x >= 0.0 && y >= 0.0 && x < f64::from(width) && y < f64::from(height);
        }
        self.widgets.accepts_pointer(x, y)
    }

    fn pointer_motion(&mut self, x: f64, y: f64) {
        self.input = true;
        self.widgets.pointer_motion(x, y);
    }

    fn pointer_leave(&mut self) {
        self.input = true;
        self.widgets.pointer_leave();
    }

    fn pointer_button(&mut self, button: u32, pressed: bool) {
        self.input = true;
        self.widgets.pointer_button(button, pressed);
    }

    fn pointer_axis(&mut self, horizontal: f64, vertical: f64) {
        self.input = true;
        self.widgets.pointer_axis(horizontal, vertical);
    }

    fn outputs_changed(&mut self, outputs: &[(i32, i32, i32, i32)]) {
        // A part not yet told which output it stands on spans all of them, as a bar stretched
        // across the tightest bounding box there is.
        if self.output.is_some() || self.retired() {
            return;
        }
        let Some((x0, y0, x1, y1)) = outputs
            .iter()
            .map(|&(x, y, w, h)| (x, y, x.saturating_add(w), y.saturating_add(h)))
            .reduce(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)))
        else {
            return;
        };
        let (Ok(width), Ok(height)) = (
            u32::try_from(x1.saturating_sub(x0)),
            u32::try_from(y1.saturating_sub(y0)),
        ) else {
            return;
        };
        self.output = Some((String::new(), (x0, y0, width, height)));
        self.place(self.edges);
    }

    fn wants_keyboard(&self) -> bool {
        // While a surface of the desk's is up it answers the keyboard: the switcher walks,
        // every surface closes on Esc, and a field in one takes what is typed. A modifier's
        // release reaches it this way too, since its press came before the surface was up.
        self.cover || self.widgets.wants_keyboard()
    }

    fn key(&mut self, key: &HostKey) {
        self.input = true;
        let Some(action) = self.surface_key(key) else {
            self.widgets.key(key);
            return;
        };
        match action {
            SurfaceKey::Walk(back) => {
                // Holding Alt opens it; a further Tab walks it on (and back with Shift).
                let steps = if back { "-1" } else { "1" };
                self.ask("walk-switcher", &[("steps", steps)]);
            }
            SurfaceKey::Commit => self.ask("commit-switcher", &[]),
            SurfaceKey::Close => {
                let up = self.origin.borrow().link.surface();
                self.ask("close-surface", &[("surface", &up)]);
            }
        }
    }

    fn cursor(&self) -> CursorIcon {
        self.widgets.cursor()
    }

    fn prepare_frame(&mut self) -> bool {
        let input = std::mem::take(&mut self.input);
        // A surface the desk shows, or a menu it opened, makes the part cover the outputs:
        // standing over them is what lets a press outside it close it.
        self.follow_cover();
        if input || self.due() {
            self.turn();
        }
        let mut drew = self.draw();
        // The toolkit takes input in at its frame: what the person pressed reports there, and
        // the instance hears of it now rather than at the next input.
        if input {
            if self.turn() {
                drew |= self.draw();
            }
            // The toolkit took the input in: a menu the part opened, or a surface of the
            // desk's, may have opened or closed with it.
            self.follow_cover();
            if self.draw() {
                drew = true;
            }
        }
        drew
    }
}

impl ShellPart {
    /// Ask the session for the desk action `action` with `arguments`: what a key of the part's
    /// own asks, as a press of the views asks through an outside effect.
    fn ask(&self, action: &str, arguments: &[(&str, &str)]) {
        let arguments = arguments
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        self.origin.borrow().link.asked.borrow_mut().push_back(Ask {
            resource: DESK_RESOURCE.to_owned(),
            action: action.to_owned(),
            arguments,
        });
    }

    /// Draw a frame of the part's widget tree when it owes one, and say whether it drew or
    /// changed what it shows. What it owes a frame for is what the views said (a turn that was
    /// shown lays the tree out), input, a motion or a deadline. A field on screen reports
    /// itself dirty every frame, so that alone draws nothing: with the centre or the launcher
    /// up and nothing changing, the part draws no frame.
    #[allow(dead_code, reason = "a test draws a frame between presses")]
    fn draw(&mut self) -> bool {
        if !self.widgets.wants_frame() {
            return false;
        }
        let before = crate::Published::revision(&self.widgets);
        let started = std::time::Instant::now();
        let drew = self.widgets.prepare_frame();
        let changed = crate::Published::revision(&self.widgets) != before;
        if changed {
            self.work.drawn = self.work.drawn.wrapping_add(1);
            self.work.drawing = self.work.drawing.saturating_add(started.elapsed());
        }
        drew || changed
    }
}

mod readings;
use readings::PlaceInvoker;

#[cfg(test)]
mod tests_support {
    #![allow(clippy::unwrap_used, clippy::expect_used, reason = "test fixtures")]

    use super::*;
    use tilo_compositor::KeyModifiers;

    /// A desk at rest with two windows and two workspaces.
    pub(super) fn reading_with_windows() -> DeskReading {
        DeskReading {
            person: "Pat".to_owned(),
            clock: "4:28 PM".to_owned(),
            workspace: "Desk".to_owned(),
            strip: "Main".to_owned(),
            windows: vec![
                WindowReading {
                    key: "11".to_owned(),
                    title: "Release planning".to_owned(),
                    origin_app: "Notes".to_owned(),
                    strip: "Main".to_owned(),
                    workspace: "Desk".to_owned(),
                    focused: true,
                    ..WindowReading::default()
                },
                WindowReading {
                    key: "12".to_owned(),
                    title: "Inbox".to_owned(),
                    origin_app: "Mail".to_owned(),
                    strip: "Main".to_owned(),
                    workspace: "Desk".to_owned(),
                    ..WindowReading::default()
                },
            ],
            ..DeskReading::default()
        }
    }

    /// A part over `reading`, its outputs set.
    pub(super) fn part_with(reading: DeskReading) -> (ShellPart, Rc<SessionLink>) {
        let packages = crate::shell_part::compile_shell().unwrap_or_else(|why| panic!("{why}"));
        let app = packages
            .last()
            .expect("the shell compiles last")
            .meta
            .app
            .clone();
        let link = Rc::new(SessionLink::default());
        link.publish(reading);
        let mut part = ShellPart::new(
            ShellIdentity {
                app: &app,
                packages,
                person: "Pat",
                link: Rc::clone(&link),
                record: Rc::new(RefCell::new(tilo_resource::Doc::new())),
                seed: true,
            },
            32,
            1.0,
            std::sync::Arc::new(|| {}),
            None,
        );
        part.outputs_changed(&[(0, 0, 1280, 800)]);
        part.set_display(Some(("HEADLESS-1".to_owned(), (0, 0, 1280, 800))));
        for _ in 0..4 {
            part.prepare_frame();
        }
        (part, link)
    }

    pub(super) fn key(keysym: u32, pressed: bool) -> HostKey {
        HostKey {
            keycode: 0,
            pressed,
            repeat: false,
            keysym,
            unmodified_keysym: keysym,
            text: None,
            modifiers: KeyModifiers::default(),
        }
    }

    /// Tab, held with Alt (`pressed` is Alt's, not Tab's); Shift walks the other way.
    pub(super) fn tab(alt: bool, shift: bool) -> HostKey {
        HostKey {
            modifiers: KeyModifiers {
                alt,
                shift,
                ..KeyModifiers::default()
            },
            ..key(0xff09, true)
        }
    }

    /// `Alt_L`, held (`true`) or let go.
    pub(super) fn alt(pressed: bool) -> HostKey {
        HostKey {
            modifiers: KeyModifiers {
                alt: pressed,
                ..KeyModifiers::default()
            },
            ..key(0xffe9, pressed)
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod places;

#[cfg(test)]
mod surface_keys {
    #![allow(clippy::unwrap_used, clippy::expect_used, reason = "test fixtures")]

    use super::tests_support::{alt, key, part_with, reading_with_windows, tab};
    use tilo_compositor::{KeyClaim, SurfaceHost as _};

    #[test]
    fn alt_tab_asks_for_the_switcher_from_rest() {
        let (mut part, link) = part_with(reading_with_windows());
        assert_ne!(
            part.claim_key(&tab(true, true), false, false),
            KeyClaim::Pass
        );
        part.key(&tab(true, true));
        assert_eq!(
            link.asked.borrow().front().map(|ask| ask.action.clone()),
            Some("walk-switcher".to_owned()),
            "Alt+Tab asks the desk for its switcher"
        );
        // Holding Alt and pressing Tab again walks the switcher on.
        link.asked.borrow_mut().clear();
        part.key(&tab(true, false));
        assert_eq!(
            link.asked.borrow().front().map(|ask| ask.action.clone()),
            Some("walk-switcher".to_owned())
        );
        assert_eq!(
            link.asked.borrow().front().map(|ask| ask.arguments.clone()),
            Some(std::collections::BTreeMap::from([(
                "steps".to_owned(),
                "1".to_owned()
            )]))
        );
    }

    #[test]
    fn letting_alt_go_commits_the_switcher() {
        let mut reading = reading_with_windows();
        reading.here = "switcher".to_owned();
        let (mut part, link) = part_with(reading);
        link.asked.borrow_mut().clear();
        assert_ne!(part.claim_key(&alt(false), false, false), KeyClaim::Pass);
        part.key(&alt(false));
        assert_eq!(
            link.asked.borrow().front().map(|ask| ask.action.clone()),
            Some("commit-switcher".to_owned()),
            "letting Alt go goes to the window the walk reached"
        );
    }

    #[test]
    fn escape_closes_the_surface_that_is_up() {
        let mut reading = super::tests_support::reading_with_windows();
        reading.here = "overview".to_owned();
        let (mut part, link) = part_with(reading);
        assert_ne!(part.claim_key(&escape(), false, false), KeyClaim::Pass);
        part.key(&escape());
        assert_eq!(
            link.asked.borrow().front().map(|ask| ask.action.clone()),
            Some("close-surface".to_owned())
        );
        assert_eq!(
            link.asked.borrow().front().map(|ask| ask.arguments.clone()),
            Some(std::collections::BTreeMap::from([(
                "surface".to_owned(),
                "overview".to_owned()
            )]))
        );
    }

    #[test]
    fn a_surface_up_makes_the_part_cover_the_outputs() {
        let mut reading = super::tests_support::reading_with_windows();
        reading.here = "launcher".to_owned();
        let (part, _link) = part_with(reading);
        assert_eq!(
            part.widgets.placement().size,
            (1280, 800),
            "the launcher has the desk's room"
        );
        assert!(
            part.accepts_pointer(640.0, 400.0),
            "the launcher takes presses"
        );
    }

    fn escape() -> tilo_compositor::HostKey {
        key(0xff1b, true)
    }
}
