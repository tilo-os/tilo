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

/// The reading of [`DESK_RESOURCE`]: the fields of the `session.desk` kind
/// (`tilolang::origin::KINDS`), as JSON on the door.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeskReading {
    pub person: String,
    /// The question the session puts to the person about themselves, waiting on their own
    /// avatar until they answer it or put it off.
    #[serde(default)]
    pub ask: Option<AskReading>,
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
    /// The others here in the workspace in front: its collaborators.
    #[serde(default)]
    pub people: Vec<PersonReading>,
}

/// The arrangement's readings, which a shell reads beside the desk's: what the person arranged.
pub use crate::arrangement::{
    ArrangementPlaceReading, ArrangementQueryReading, ArrangementReading, ArrangementRefReading,
    ArrangementRowReading, ArrangementStripReading, ArrangementWorkspaceReading,
};

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

/// What a shell asked of the session: an invocation on the door, its arguments as any caller's
/// reach it (a list as a list, a map as an object).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ask {
    pub resource: String,
    pub action: String,
    pub arguments: BTreeMap<String, serde_json::Value>,
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
