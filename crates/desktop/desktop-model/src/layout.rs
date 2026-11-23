//! The layout: its window topology (strips of window groups of tabs) and how a workspace shows
//! its windows — in strips, or on a canvas.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::app::AppKind;
use crate::ids::{GroupId, StripId, TabId};
use crate::width::WindowWidth;

/// What a tile holds. What is stored is the form that opens again: an app
/// ([`TileContent::LocalApp`]), a placeholder, a stub, or a window hosted on
/// another device — never a live foreign surface, whose id names a buffer
/// only the compositor that mapped it owns.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum TileContent {
    AppPlaceholder {
        kind: AppKind,
        label: String,
    },
    Stub {
        kind: AppKind,
        label: String,
    },
    /// Foreign Wayland client adopted by the compositor. `surface_id` is
    /// the `wl_surface` protocol id the compositor maps back to a smithay
    /// `WlSurface`; renders as a Stub until the frame loop blits it.
    Foreign {
        app_id: String,
        surface_id: u64,
    },
    /// Durable local application window awaiting a fresh compositor surface.
    /// Unlike `Foreign`, this stores no process-local Wayland id. At session
    /// startup the normal launcher opens `app_id` and binds its first
    /// toplevel back into this exact tab, preserving layout without reviving
    /// a dead surface handle.
    LocalApp {
        app_id: String,
        label: String,
        #[serde(default)]
        note: String,
    },
    /// A window that runs only on the device `host` (its hex device key),
    /// shown on every other device as that window, never relaunched there:
    /// what it shows lives in its process on that device, not in a record
    /// another device could open. A legacy (X11 / Wayland) window is one,
    /// and so is a window of an app that says so of its windows (a
    /// browser's page lives in its engine). The tab id and `host` stay
    /// stable for the window's life, so a per-window stream from the host
    /// can attach to this tab. On the hosting device the same tab is its
    /// live [`Self::Foreign`] surface.
    HostedWindow {
        /// The wayland app-id or X11 class the window announces.
        app_id: String,
        /// The window's title on the hosting device.
        label: String,
        /// Hex device key (iroh endpoint id) of the hosting device.
        host: String,
        /// The hosting device's own name, as its certificate labels it.
        #[serde(default)]
        host_label: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tab {
    pub id: TabId,
    /// What the tab shows: the name a person gave it, else what its window
    /// shows — the app's name for that, or the window's own title.
    pub title: String,
    pub tile: TileContent,
    /// The name the tab goes back to when the person's own name is cleared:
    /// the app's name for what the window shows, else the window's own
    /// latest title. `None` while no person's name stands in `title`.
    /// Written only by [`Tab::set_window_title`] and [`Tab::rename`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_title: Option<String>,
    /// The view the tab shows its record through, or the item its window
    /// opened on, on every device; empty for the app's own default view.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub view: String,
    /// Whether the person pinned the tab: an agent arranges a pinned tab
    /// nowhere, so what the person fixed stays where they put it.
    #[serde(default, skip_serializing_if = "is_unpinned")]
    pub pinned: bool,
}

/// Whether a flag is off: a flag that is off is not written at all.
#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes a reference"
)]
fn is_unpinned(pinned: &bool) -> bool {
    !*pinned
}

impl Tab {
    /// The window is called `title` now: its own title, or, while its app
    /// names what the window shows, that name — the shell resolves the two.
    /// While a person's name stands, it is kept for when the name is cleared,
    /// and the tab goes on showing the name. A window that names nothing
    /// keeps the name it has: a bar with no title names nothing to the
    /// person looking at the layout. Returns whether anything changed: a window
    /// sending the same title again (terminals do, continuously) is no
    /// change.
    pub fn set_window_title(&mut self, title: &str) -> bool {
        if title.trim().is_empty() {
            return false;
        }
        let shown = self.app_title.as_mut().unwrap_or(&mut self.title);
        if shown == title {
            return false;
        }
        title.clone_into(shown);
        true
    }

    /// The window says it belongs to `app_id` now: an X11 client renamed its
    /// class, or an app set its id after its window opened. The tab belongs
    /// to that app from here on. Returns whether it is another app than the
    /// one the tab holds: a window claiming the same app again (clients
    /// re-send it) is no change.
    pub fn set_window_app_id(&mut self, app_id: &str) -> bool {
        let TileContent::Foreign { app_id: held, .. } = &mut self.tile else {
            return false;
        };
        if held == app_id {
            return false;
        }
        app_id.clone_into(held);
        true
    }

    /// A person names the tab `name`. A blank name clears their name, and
    /// the tab shows its window's latest title again. Returns whether
    /// anything changed.
    pub fn rename(&mut self, name: &str) -> bool {
        let name = name.trim();
        if name.is_empty() {
            return match self.app_title.take() {
                Some(window_title) => {
                    self.title = window_title;
                    true
                }
                None => false,
            };
        }
        match &self.app_title {
            Some(_) if self.title == name => false,
            Some(_) => {
                name.clone_into(&mut self.title);
                true
            }
            None => {
                self.app_title = Some(std::mem::replace(&mut self.title, name.to_owned()));
                true
            }
        }
    }

    /// Whether a person's name stands in place of the window's own title.
    #[must_use]
    pub fn is_named(&self) -> bool {
        self.app_title.is_some()
    }

    /// The tab as it opens again after this process ended. A running window's
    /// surface belongs to the compositor that mapped it, and the same number
    /// after a restart is another window or none, so only the app that opens
    /// it again is kept ([`TileContent::LocalApp`]); its title stands in as
    /// the label until the app reports one. Content that already opens again
    /// — a local app, a placeholder, a stub, a window hosted on another
    /// device — is kept as it is.
    #[must_use]
    pub fn relaunchable(&self) -> Self {
        let tile = match &self.tile {
            TileContent::Foreign { app_id, .. } => TileContent::LocalApp {
                app_id: app_id.clone(),
                label: self.title.clone(),
                note: String::new(),
            },
            other => other.clone(),
        };
        Self {
            id: self.id,
            title: self.title.clone(),
            tile,
            app_title: self.app_title.clone(),
            view: self.view.clone(),
            pinned: self.pinned,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowGroup {
    pub id: GroupId,
    pub tabs: Vec<Tab>,
    pub active_tab: TabId,
    pub width: WindowWidth,
    /// Mirrors the widget's maximized flag. This is presentation state for
    /// this device: it survives a local restart but is never sent to workspace
    /// peers. `#[serde(default)]` keeps pre-existing local JSON loading
    /// un-maximized.
    #[serde(default)]
    pub maximized: bool,
    /// The tabs that were in front before the current one, most recent last.
    /// Like `active_tab` itself this is what this device is showing: never
    /// replicated or persisted. Closing the front tab goes back to the most
    /// recent of these that is still open. Written only by
    /// [`WindowGroup::activate_tab`].
    #[serde(skip)]
    pub focus_history: Vec<TabId>,
}

impl WindowGroup {
    /// Bring `tab` to the front, remembering the tab it replaces. The one
    /// place a *selection* is recorded, whoever makes it (a click, a
    /// keyboard shortcut, a protocol activation, a new window opening).
    /// Does nothing if `tab` is already in front.
    pub fn activate_tab(&mut self, tab: TabId) {
        if self.active_tab == tab {
            return;
        }
        let previous = self.active_tab;
        if self.tabs.iter().any(|t| t.id == previous) {
            self.focus_history.retain(|t| *t != previous);
            self.focus_history.push(previous);
        }
        self.focus_history.retain(|t| *t != tab);
        self.active_tab = tab;
        // Only tabs still in the group can be returned to.
        let tabs = &self.tabs;
        self.focus_history
            .retain(|t| tabs.iter().any(|x| x.id == *t));
    }

    /// The tab to show after `closed` goes away: the most recently shown one
    /// that is still open, if any.
    pub fn previously_active(&self, closed: TabId) -> Option<TabId> {
        self.focus_history
            .iter()
            .rev()
            .copied()
            .find(|t| *t != closed && self.tabs.iter().any(|x| x.id == *t))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Strip {
    pub id: StripId,
    pub name: String,
    pub window_groups: Vec<WindowGroup>,
}

impl Strip {
    pub fn new(id: StripId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            window_groups: Vec::new(),
        }
    }
}

/// Which layout a workspace shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LayoutKind {
    /// Rows of windows, one strip at a time.
    #[default]
    Strips,
    /// Windows anywhere on a pannable, zoomable surface.
    Canvas,
}

impl LayoutKind {
    /// The kind as the shared record and the desktop document store it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Strips => "strips",
            Self::Canvas => "canvas",
        }
    }

    /// The stored kind; anything this build does not know reads as strips,
    /// so a newer layout never leaves an older device without one.
    #[must_use]
    pub fn parse(stored: &str) -> Self {
        match stored {
            "canvas" => Self::Canvas,
            _ => Self::Strips,
        }
    }
}

/// The least a window stands at wherever the workspace keeps its size (on the
/// canvas or floating), in logical pixels: as wide as a window in a strip can
/// get ([`crate::width::WIDTH_FLOOR_PX`]: its tab pill, its controls and a
/// scrollbar), and as tall as its bar with a few lines under it. Every member,
/// person or agent, writes the shared record, so a size below it is never
/// kept: it is raised when written and when read back, and a window laid out
/// from a place smaller than this (one a build without the floor wrote) still
/// stands at it.
pub const WINDOW_MIN_WIDTH: u32 = 160;
/// See [`WINDOW_MIN_WIDTH`].
pub const WINDOW_MIN_HEIGHT: u32 = 120;

/// Where a window group stands on the canvas, in canvas pixels: its top-left
/// corner and its size.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct CanvasPlace {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// Its height is the most it takes: what it shows is as tall as what it holds up to it (a
    /// thing found where the person clicked, which names no size of its own). A place the person
    /// sized — a rectangle they dragged, an edge they pulled — keeps its height exactly.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hugs: bool,
    /// How tall a hugging place is drawn now, in canvas pixels, as the person's own canvas
    /// reports it: the height what it shows comes to, up to [`Self::height`]. View state, never
    /// an arranging step and never written: it names no size the person gave, the canvas that
    /// draws the place reports it again, and a place that stops hugging drops it.
    #[serde(skip)]
    pub drawn_height: Option<u32>,
}

impl PartialEq for CanvasPlace {
    fn eq(&self, other: &Self) -> bool {
        self.x == other.x
            && self.y == other.y
            && self.width == other.width
            && self.height == other.height
            && self.hugs == other.hugs
    }
}

impl Eq for CanvasPlace {}

impl std::hash::Hash for CanvasPlace {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        (self.x, self.y, self.width, self.height, self.hugs).hash(state);
    }
}

impl CanvasPlace {
    #[must_use]
    pub fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
            hugs: false,
            drawn_height: None,
        }
    }

    /// This place, as tall as what it shows up to its height ([`Self::hugs`]).
    #[must_use]
    pub fn hugging(mut self) -> Self {
        self.hugs = true;
        self
    }

    /// The area of the canvas this place takes now: its full height, or a hugging place's height
    /// as it is drawn when its canvas has reported one. Every free-area check stands a place
    /// against this, so what the person sees free is what is free (`canvas.md` §1, §3, §7).
    #[must_use]
    pub fn occupied(&self) -> Self {
        let mut occupied = Self {
            drawn_height: None,
            ..*self
        };
        if let Some(drawn) = self.drawn_height.filter(|_| self.hugs) {
            occupied.height = drawn.min(self.height).max(1);
        }
        occupied
    }

    /// Report how tall this hugging place is drawn now, in canvas pixels: what its canvas sees of
    /// what it shows, up to its height. A place that does not hug keeps no drawn height. Whether
    /// anything changed.
    pub fn drawn(&mut self, height: u32) -> bool {
        let height = self.hugs.then(|| height.clamp(1, self.height));
        if self.drawn_height == height {
            return false;
        }
        self.drawn_height = height;
        true
    }

    /// This place as a window's: no smaller than [`WINDOW_MIN_WIDTH`] ×
    /// [`WINDOW_MIN_HEIGHT`], from the same corner. A place that is an area (a
    /// selection, the bounds of several windows) keeps its own size.
    #[must_use]
    pub fn window(self) -> Self {
        Self {
            width: self.width.max(WINDOW_MIN_WIDTH),
            height: self.height.max(WINDOW_MIN_HEIGHT),
            ..self
        }
    }

    /// The right edge, exclusive.
    #[must_use]
    pub fn right(&self) -> i64 {
        i64::from(self.x).saturating_add(i64::from(self.width))
    }

    /// The bottom edge, exclusive.
    #[must_use]
    pub fn bottom(&self) -> i64 {
        i64::from(self.y).saturating_add(i64::from(self.height))
    }

    /// Whether the two overlap, or come closer than `gap` to each other.
    #[must_use]
    pub fn crowds(&self, other: &Self, gap: u32) -> bool {
        self.crowds_apart(other, gap, gap)
    }

    /// Whether the two overlap, or come closer than `across` side by side or `down` one over the
    /// other.
    #[must_use]
    pub fn crowds_apart(&self, other: &Self, across: u32, down: u32) -> bool {
        let (across, down) = (i64::from(across), i64::from(down));
        i64::from(self.x) < other.right().saturating_add(across)
            && i64::from(other.x) < self.right().saturating_add(across)
            && i64::from(self.y) < other.bottom().saturating_add(down)
            && i64::from(other.y) < self.bottom().saturating_add(down)
    }

    /// Whether the two stand too close on a canvas: closer than [`CANVAS_GAP`] side by side, or
    /// than [`CANVAS_ROW_GAP`] one over the other, so the caption over the lower one is never
    /// under the upper one. A hugging place takes the height it is drawn at, not the most it
    /// could grow to ([`Self::occupied`]): this is the one place the canvas's crowding rule
    /// reads it.
    #[must_use]
    pub fn crowds_on_canvas(&self, other: &Self) -> bool {
        self.occupied()
            .crowds_apart(&other.occupied(), CANVAS_GAP, CANVAS_ROW_GAP)
    }

    /// Whether this place stands wholly inside `area`.
    #[must_use]
    pub fn inside(&self, area: &Self) -> bool {
        self.x >= area.x
            && self.y >= area.y
            && self.right() <= area.right()
            && self.bottom() <= area.bottom()
    }

    /// Whether the two share any area.
    #[must_use]
    pub fn intersects(&self, other: &Self) -> bool {
        self.crowds(other, 0)
    }

    /// The point of this place nearest `(x, y)`, and how far that is,
    /// squared: zero inside it.
    #[must_use]
    pub fn distance_sq(&self, (x, y): (i32, i32)) -> i64 {
        let cx = i64::from(x).clamp(i64::from(self.x), self.right());
        let cy = i64::from(y).clamp(i64::from(self.y), self.bottom());
        let dx = i64::from(x).saturating_sub(cx);
        let dy = i64::from(y).saturating_sub(cy);
        dx.saturating_mul(dx).saturating_add(dy.saturating_mul(dy))
    }
}

/// The space kept between two windows on the canvas, in canvas pixels: the
/// design language's `group` rhythm.
pub const CANVAS_GAP: u32 = 24;

/// The band a place keeps over its top edge on the canvas for its caption and its handle, in
/// canvas pixels: the toolkit's place band (`ui::widget::canvas_place::PLACE_BAND`). A place's
/// point is its view's top, and the band stands above it.
pub const CANVAS_BAND: u32 = 44;

/// The space kept between a place and the one under it on the canvas: the lower one's band and
/// the canvas's gap, so its caption stands clear of the place above it.
pub const CANVAS_ROW_GAP: u32 = CANVAS_GAP + CANVAS_BAND;

/// The window a group the shell opened takes on the canvas when nobody named a place for it:
/// the framed canvas's window, in canvas pixels, which is what the layout's own place
/// action defaults to for a place a person types at.
const MISSING_PLACE_WIDTH: u32 = 720;
/// See [`MISSING_PLACE_WIDTH`].
const MISSING_PLACE_HEIGHT: u32 = 520;

/// The corner of the work area a floated window keeps its distance from, so
/// the window lands sensibly on every member's screen size.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum FloatCorner {
    TopLeft,
    TopRight,
    /// The corner a window floats to when the person names none, as cards
    /// do.
    #[default]
    BottomRight,
    BottomLeft,
    /// A corner a newer build stores that this one does not know: the window
    /// still floats, kept at the default corner.
    #[serde(other)]
    Unknown,
}

/// Where a window floats: its corner of the work area, the distance from the
/// two edges meeting there, and its size, all in whole logical pixels.
///
/// Anchoring to a corner rather than to the screen's top-left keeps a float
/// where the person put it on a display of any size: a window 40 px from the
/// bottom-right is 40 px from the bottom-right of everyone's work area.
///
/// Its size is a window's: never under [`WINDOW_MIN_WIDTH`] ×
/// [`WINDOW_MIN_HEIGHT`], however it was made or written.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "StoredFloat")]
pub struct FloatPlace {
    #[serde(default)]
    pub corner: FloatCorner,
    /// Distance from the corner's left or right edge.
    pub dx: i32,
    /// Distance from the corner's top or bottom edge.
    pub dy: i32,
    pub width: u32,
    pub height: u32,
}

/// A [`FloatPlace`] as the record holds it, before its size is a window's.
#[derive(Deserialize)]
struct StoredFloat {
    #[serde(default)]
    corner: FloatCorner,
    dx: i32,
    dy: i32,
    width: u32,
    height: u32,
}

impl From<StoredFloat> for FloatPlace {
    /// A float read back is a window's size, whoever wrote a smaller one: a
    /// sliver kept in the record is a window everyone sees again.
    fn from(stored: StoredFloat) -> Self {
        Self::new(
            stored.corner,
            stored.dx,
            stored.dy,
            stored.width,
            stored.height,
        )
    }
}

impl FloatPlace {
    /// A float of `width` × `height`, raised to a window's least size.
    #[must_use]
    pub fn new(corner: FloatCorner, dx: i32, dy: i32, width: u32, height: u32) -> Self {
        Self {
            corner,
            dx,
            dy,
            width: width.max(WINDOW_MIN_WIDTH),
            height: height.max(WINDOW_MIN_HEIGHT),
        }
    }

    /// The float of a window of `width` × `height` let go as `rect` on the
    /// work area `area`: anchored to the corner `rect` lies nearest, at the
    /// distance it was let go from that corner's edges.
    #[must_use]
    pub fn of_rect(area: CanvasPlace, rect: CanvasPlace) -> Self {
        let centre = (
            rect.x
                .saturating_add(i32::try_from(rect.width / 2).unwrap_or(i32::MAX)),
            rect.y
                .saturating_add(i32::try_from(rect.height / 2).unwrap_or(i32::MAX)),
        );
        let right = i32::try_from(area.right()).unwrap_or(i32::MAX);
        let bottom = i32::try_from(area.bottom()).unwrap_or(i32::MAX);
        let corners = [
            FloatCorner::TopLeft,
            FloatCorner::TopRight,
            FloatCorner::BottomRight,
            FloatCorner::BottomLeft,
        ];
        let corner = corners
            .into_iter()
            .min_by_key(|corner| {
                let at = corner_point(*corner, area);
                let dx = i64::from(at.0).saturating_sub(i64::from(centre.0));
                let dy = i64::from(at.1).saturating_sub(i64::from(centre.1));
                (
                    dx.saturating_mul(dx).saturating_add(dy.saturating_mul(dy)),
                    *corner,
                )
            })
            .unwrap_or_default();
        let dx = if corner.is_right() {
            right.saturating_sub(
                rect.x
                    .saturating_add(i32::try_from(rect.width).unwrap_or(0)),
            )
        } else {
            rect.x.saturating_sub(area.x)
        };
        let dy = if corner.is_bottom() {
            bottom.saturating_sub(
                rect.y
                    .saturating_add(i32::try_from(rect.height).unwrap_or(0)),
            )
        } else {
            rect.y.saturating_sub(area.y)
        };
        Self::new(corner, dx.max(0), dy.max(0), rect.width, rect.height)
    }

    /// The window's rect on the work area `area`: its size, never larger
    /// than the area, at its distance from the corner, kept inside the area.
    #[must_use]
    pub fn resolved(&self, area: CanvasPlace) -> CanvasPlace {
        // A window's least size, unless the work area itself is smaller.
        let width = self.width.max(WINDOW_MIN_WIDTH).min(area.width.max(1));
        let height = self.height.max(WINDOW_MIN_HEIGHT).min(area.height.max(1));
        let spare = |total: u32, size: u32| i32::try_from(total.saturating_sub(size)).unwrap_or(0);
        let dx = self.dx.max(0).min(spare(area.width, width));
        let dy = self.dy.max(0).min(spare(area.height, height));
        let x = if self.corner.is_right() {
            area.x
                .saturating_add(spare(area.width, width))
                .saturating_sub(dx)
        } else {
            area.x.saturating_add(dx)
        };
        let y = if self.corner.is_bottom() {
            area.y
                .saturating_add(spare(area.height, height))
                .saturating_sub(dy)
        } else {
            area.y.saturating_add(dy)
        };
        CanvasPlace::new(x, y, width, height)
    }

    /// The face of the window nearest its corner, as the corner's distance
    /// from the corner's own edges (0 = flush with them): what a resize or a
    /// drag keeps while the window changes size around that face.
    #[must_use]
    pub fn at(&self, dx: i32, dy: i32) -> Self {
        Self {
            dx: dx.max(0),
            dy: dy.max(0),
            ..*self
        }
    }
}

impl FloatCorner {
    /// Whether the corner is on the work area's right side.
    #[must_use]
    pub fn is_right(self) -> bool {
        matches!(self, Self::TopRight | Self::BottomRight)
    }

    /// Whether the corner is on the work area's bottom side.
    #[must_use]
    pub fn is_bottom(self) -> bool {
        matches!(self, Self::BottomLeft | Self::BottomRight)
    }

    /// Its name where a person or an agent names it: an action's argument and
    /// a shell's reading of the layout.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TopLeft => "top-left",
            Self::TopRight => "top-right",
            Self::BottomRight | Self::Unknown => "bottom-right",
            Self::BottomLeft => "bottom-left",
        }
    }

    /// The corner `text` names; `None` for a name this build does not know.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "top-left" => Some(Self::TopLeft),
            "top-right" => Some(Self::TopRight),
            "bottom-right" => Some(Self::BottomRight),
            "bottom-left" => Some(Self::BottomLeft),
            _ => None,
        }
    }
}

/// The point of the work area `area` a corner names.
fn corner_point(corner: FloatCorner, area: CanvasPlace) -> (i32, i32) {
    let right = i32::try_from(area.right()).unwrap_or(i32::MAX);
    let bottom = i32::try_from(area.bottom()).unwrap_or(i32::MAX);
    (
        if corner.is_right() { right } else { area.x },
        if corner.is_bottom() { bottom } else { area.y },
    )
}

/// A workspace's layout: which kind it shows and where its groups stand
/// on the canvas. Places are kept while the strips show, so going back to
/// the canvas finds every window where it was left.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceLayout {
    #[serde(default)]
    pub kind: LayoutKind,
    // Read through the id-key helper: a workspace is stored flattened, where
    // a map's numeric keys arrive as strings.
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        deserialize_with = "deserialize_window_places"
    )]
    pub places: BTreeMap<GroupId, CanvasPlace>,
    /// Where each floated window stands: a window the person lifted off the
    /// grid, over it and on every member's screen. The group keeps its strip
    /// and order, so snapping it back returns it exactly where it was.
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        deserialize_with = "crate::serde_ids::deserialize_id_btree_map"
    )]
    pub floats: BTreeMap<GroupId, FloatPlace>,
}

impl WorkspaceLayout {
    /// Whether this is the layout a workspace has before anyone chose
    /// one: strips, nothing placed, nothing floating.
    #[must_use]
    pub fn is_default(&self) -> bool {
        self.kind == LayoutKind::Strips && self.places.is_empty() && self.floats.is_empty()
    }

    /// Where `group` stands, once placed.
    #[must_use]
    pub fn place_of(&self, group: GroupId) -> Option<CanvasPlace> {
        self.places.get(&group).copied()
    }

    /// Where `group` floats, while it does.
    #[must_use]
    pub fn float_of(&self, group: GroupId) -> Option<FloatPlace> {
        self.floats.get(&group).copied()
    }

    /// Stand `group` at `(x, y)` on the canvas, keeping its size and every other place where it
    /// is: what a drag that let a place go on the canvas commits, one arranging action for the
    /// whole gesture. Nothing placed moves. Whether the place stood somewhere else.
    pub fn set_position(&mut self, group: GroupId, x: i32, y: i32) -> bool {
        let Some(place) = self.places.get_mut(&group) else {
            return false;
        };
        if place.x == x && place.y == y {
            return false;
        }
        place.x = x;
        place.y = y;
        true
    }

    /// Give `group` the size `width` × `height` on the canvas, keeping its corner where it
    /// stands. Returns whether it changed.
    pub fn set_size(&mut self, group: GroupId, width: u32, height: u32) -> bool {
        let Some(place) = self.places.get_mut(&group) else {
            return false;
        };
        if place.width == width && place.height == height && !place.hugs {
            return false;
        }
        place.width = width;
        place.height = height;
        // Sized by hand, it keeps the size it was given: it no longer hugs, and how tall it was
        // drawn is not its size.
        place.hugs = false;
        place.drawn_height = None;
        true
    }

    /// Report how tall the hugging place `group` is drawn now, `height` canvas pixels: what its
    /// canvas sees of what it shows, up to the place's height. A place that does not hug, and one
    /// the layout does not hold, keeps nothing. Whether the layout changed.
    pub fn set_drawn_height(&mut self, group: GroupId, height: u32) -> bool {
        self.places
            .get_mut(&group)
            .is_some_and(|place| place.drawn(height))
    }

    /// Float `group` where `float` says (`None` snaps it back into the grid).
    /// Returns whether it changed.
    pub fn set_float(&mut self, group: GroupId, float: Option<FloatPlace>) -> bool {
        let float = float.map(|float| {
            FloatPlace::new(float.corner, float.dx, float.dy, float.width, float.height)
        });
        match float {
            Some(float) => self.floats.insert(group, float) != Some(float),
            None => self.floats.remove(&group).is_some(),
        }
    }

    /// The free place of `width` × `height` nearest `anchor` (the corner a
    /// window placed there would have), at least [`CANVAS_GAP`] from every
    /// placed group but `except`. Nothing placed moves. Deterministic: the
    /// same layout and anchor give the same place on every device.
    #[must_use]
    pub fn free_place_near(
        &self,
        anchor: (i32, i32),
        width: u32,
        height: u32,
        except: Option<GroupId>,
    ) -> CanvasPlace {
        self.nearest_free(anchor, (width, height), except, None)
            .unwrap_or_else(|| {
                // Every candidate crowded something (cannot happen with a finite set: right of
                // the rightmost is always free); go there.
                let right = self
                    .places
                    .iter()
                    .filter(|(id, _)| Some(**id) != except)
                    .map(|(_, place)| place.occupied().right())
                    .max()
                    .unwrap_or(0);
                let x = right.saturating_add(i64::from(CANVAS_GAP));
                CanvasPlace::new(
                    i32::try_from(x).unwrap_or(i32::MAX),
                    anchor.1,
                    width,
                    height,
                )
            })
    }

    /// The free place nearest `anchor` that stands whole inside `seen` (the part of the canvas
    /// the person sees), at least [`CANVAS_GAP`] from every placed group: `width` × `height`, or
    /// shrunk to fit down to a window's least size, the largest first. Where something asked at a
    /// point with no free area stands, so the camera does not pan away from what the person was
    /// looking at. `None` when not even a window's least size is free in `seen`. Nothing placed
    /// moves.
    #[must_use]
    pub fn free_place_in_view(
        &self,
        anchor: (i32, i32),
        size: (u32, u32),
        seen: CanvasPlace,
    ) -> Option<CanvasPlace> {
        shrinking(size).find_map(|size| self.nearest_free(anchor, size, None, Some(seen)))
    }

    /// The free place of `width` × `height` nearest `anchor`, clear of every placed group but
    /// `except` and, given `within`, whole inside it. Deterministic: the same layout and
    /// anchor give the same place on every device.
    fn nearest_free(
        &self,
        anchor: (i32, i32),
        (width, height): (u32, u32),
        except: Option<GroupId>,
        within: Option<CanvasPlace>,
    ) -> Option<CanvasPlace> {
        let others: Vec<CanvasPlace> = self
            .places
            .iter()
            .filter(|(id, _)| Some(**id) != except)
            .map(|(_, place)| place.occupied())
            .collect();
        let inside = |place: &CanvasPlace| {
            within.is_none_or(|within| {
                place.x >= within.x
                    && place.y >= within.y
                    && place.right() <= within.right()
                    && place.bottom() <= within.bottom()
            })
        };
        let free = |place: &CanvasPlace| {
            inside(place) && !others.iter().any(|o| place.crowds_on_canvas(o))
        };
        let wanted = CanvasPlace::new(anchor.0, anchor.1, width, height);
        if free(&wanted) {
            return Some(wanted);
        }
        // Every free place nearest the anchor touches an obstacle's edge, the edge of `within`,
        // or the anchor's own line: try each obstacle's four sides, on the anchor's line and
        // flush with the obstacle's edges.
        let gap = i64::from(CANVAS_GAP);
        let row_gap = i64::from(CANVAS_ROW_GAP);
        let (w, h) = (i64::from(width), i64::from(height));
        let mut xs = vec![i64::from(anchor.0)];
        let mut ys = vec![i64::from(anchor.1)];
        if let Some(within) = within {
            xs.extend([i64::from(within.x), within.right().saturating_sub(w)]);
            ys.extend([i64::from(within.y), within.bottom().saturating_sub(h)]);
        }
        for o in &others {
            xs.extend([
                o.right().saturating_add(gap),
                i64::from(o.x).saturating_sub(gap).saturating_sub(w),
                i64::from(o.x),
                o.right().saturating_sub(w),
            ]);
            ys.extend([
                o.bottom().saturating_add(row_gap),
                i64::from(o.y).saturating_sub(row_gap).saturating_sub(h),
                i64::from(o.y),
                o.bottom().saturating_sub(h),
            ]);
        }
        let clamp = |v: i64| i32::try_from(v).unwrap_or(if v < 0 { i32::MIN } else { i32::MAX });
        let mut best: Option<(i64, CanvasPlace)> = None;
        for &x in &xs {
            for &y in &ys {
                let place = CanvasPlace::new(clamp(x), clamp(y), width, height);
                if !free(&place) {
                    continue;
                }
                let dx = x.saturating_sub(i64::from(anchor.0));
                let dy = y.saturating_sub(i64::from(anchor.1));
                let distance = dx.saturating_mul(dx).saturating_add(dy.saturating_mul(dy));
                if best.is_none_or(|(d, b)| (distance, b.y, b.x) < (d, place.y, place.x)) {
                    best = Some((distance, place));
                }
            }
        }
        best.map(|(_, place)| place)
    }

    /// How wide a window `height` tall stands with its top-left at `at` (a click) in the free
    /// room to its right: `want`, narrowed to stand [`CANVAS_GAP`] clear of the next place to its
    /// right, never under [`WINDOW_MIN_WIDTH`]. `None` when not even that fits there (a place
    /// stands at the point, or too close to its right): the caller takes the nearest free spot
    /// ([`Self::free_place_near`]). Nothing placed moves.
    #[must_use]
    pub fn free_width_at(&self, at: (i32, i32), want: u32, height: u32) -> Option<u32> {
        let want = want.max(WINDOW_MIN_WIDTH);
        let gap = i64::from(CANVAS_GAP);
        let probe = CanvasPlace::new(at.0, at.1, want, height);
        let mut width = i64::from(want);
        for other in self.places.values() {
            if !probe.crowds_on_canvas(other) {
                continue;
            }
            // Crowding it at its full width, it stands on the place's line: the room up to the
            // place's leading edge, less the gap, is all it may take.
            let room = i64::from(other.x)
                .saturating_sub(gap)
                .saturating_sub(i64::from(at.0));
            width = width.min(room);
        }
        u32::try_from(width)
            .ok()
            .filter(|width| *width >= WINDOW_MIN_WIDTH)
    }

    /// The free place beside `subject` (a window, a selection) for a window
    /// of `width` × `height`: to its trailing side when there is room
    /// there, else the nearest free space.
    #[must_use]
    pub fn free_place_beside(
        &self,
        subject: CanvasPlace,
        width: u32,
        height: u32,
        except: Option<GroupId>,
    ) -> CanvasPlace {
        let gap = i64::from(CANVAS_GAP);
        let x = i32::try_from(subject.right().saturating_add(gap)).unwrap_or(i32::MAX);
        self.free_place_near((x, subject.y), width, height, except)
    }

    /// The free place next to `subject` for a window of `width` × `height`:
    /// to its trailing side at its own line, then below it, then below its
    /// trailing side, and onward in that order down the two columns the anchor
    /// starts — never above it or to its leading side, so a place opened beside
    /// what a person is in stands where they look rather than off the top or
    /// the left of the canvas. Nothing placed moves, and every candidate is
    /// [`CANVAS_GAP`] clear of everything but `except`.
    ///
    /// Deterministic: the same layout and anchor give the same place on
    /// every device. A canvas crowded below both columns continues down the
    /// trailing column, where the canvas pans.
    #[must_use]
    pub fn free_place_next_to(
        &self,
        subject: CanvasPlace,
        width: u32,
        height: u32,
        except: Option<GroupId>,
    ) -> CanvasPlace {
        let others: Vec<CanvasPlace> = self
            .places
            .iter()
            .filter(|(id, _)| Some(**id) != except)
            .map(|(_, place)| place.occupied())
            .collect();
        let free = |place: &CanvasPlace| !others.iter().any(|o| place.crowds_on_canvas(o));
        let gap = i64::from(CANVAS_GAP);
        let row_gap = i64::from(CANVAS_ROW_GAP);
        let step_y = i64::from(height).saturating_add(row_gap);
        let right = subject.right().saturating_add(gap);
        let below = subject.bottom().saturating_add(row_gap);
        let clamp = |v: i64| i32::try_from(v).unwrap_or(if v < 0 { i32::MIN } else { i32::MAX });
        // Past every obstacle's foot both columns are free: the walk stops there.
        let foot = others
            .iter()
            .map(CanvasPlace::bottom)
            .max()
            .unwrap_or(0)
            .saturating_add(row_gap);
        // The trailing side at the anchor's own line first: what a person opening something
        // beside where they are expects to see.
        for (x, y) in [
            (right, i64::from(subject.y)),
            (i64::from(subject.x), below),
            (right, below),
        ] {
            let place = CanvasPlace::new(clamp(x), clamp(y), width, height);
            if free(&place) {
                return place;
            }
        }
        // One row down at a time, along the two columns the anchor starts, until both stand past
        // every obstacle's foot: `step_y` is at least the gap, so the walk always ends.
        let mut drop = step_y;
        while drop <= foot.saturating_sub(below) {
            for (x, y) in [
                (right, i64::from(subject.y).saturating_add(drop)),
                (i64::from(subject.x), below.saturating_add(drop)),
            ] {
                let place = CanvasPlace::new(clamp(x), clamp(y), width, height);
                if free(&place) {
                    return place;
                }
            }
            drop = drop.saturating_add(step_y);
        }
        // Every row the obstacles reach is taken: continue down the trailing column.
        let mut y = below;
        loop {
            let place = CanvasPlace::new(clamp(right), clamp(y), width, height);
            if free(&place) || y == i64::MAX {
                return place;
            }
            y = y.saturating_add(step_y);
        }
    }

    /// Where what an answer shows stands beside its turn `subject`, for a window of `width` ×
    /// `height` (canvas.md §3, §7): in the free spot adjacent to the turn, on its trailing side,
    /// then below it, then on its leading side, then above it. A spot that stands on the screen
    /// (`seen`'s size) together with the turn comes first, made smaller to fit, the one on the
    /// turn's own line first: the camera pans to show both. Failing that, the largest part of the
    /// window that is free beside the turn, never under a window's least size, the spot most
    /// inside what the person sees first: `seen` (the part of the canvas they saw when they
    /// asked), panned the least that shows the turn whole, since the camera keeps the turn. Before
    /// all of these, a spot on any side that stands whole in that view, so nothing pans. Nothing
    /// placed moves, and every spot is clear of everything but `except`
    /// ([`CanvasPlace::crowds_on_canvas`]). `None` when no spot adjacent to the turn is free: the
    /// caller stands it further along ([`Self::free_place_next_to`]).
    #[must_use]
    pub fn free_place_within(
        &self,
        subject: CanvasPlace,
        (width, height): (u32, u32),
        seen: CanvasPlace,
        except: Option<GroupId>,
    ) -> Option<CanvasPlace> {
        let others: Vec<CanvasPlace> = self
            .places
            .iter()
            .filter(|(id, _)| Some(**id) != except)
            .map(|(_, place)| place.occupied())
            .collect();
        let free = |place: &CanvasPlace| !others.iter().any(|o| place.crowds_on_canvas(o));
        let beside = Beside::new(subject, seen);
        // In what the person sees first, on any side, shrunk to fit: nothing pans, and what they
        // were looking at stays in view.
        for side in [Side::Trailing, Side::Below, Side::Leading, Side::Above] {
            let Some(asked) = beside.size(side, (width, height), true) else {
                continue;
            };
            for size in shrinking(asked) {
                let best = beside
                    .spots(side, size, &others)
                    .filter(|place| free(place) && beside.in_view(place))
                    .max_by_key(|place| beside.rank(side, place, true));
                if best.is_some() {
                    return best;
                }
            }
        }
        // Then on the screen with the turn, shrunk to fit, the camera panning to show both; then
        // as large as is free beside it.
        for on_screen in [true, false] {
            for side in [Side::Trailing, Side::Below, Side::Leading, Side::Above] {
                let Some(asked) = beside.size(side, (width, height), on_screen) else {
                    continue;
                };
                for size in shrinking(asked) {
                    let best = beside
                        .spots(side, size, &others)
                        .filter(|place| free(place) && (!on_screen || beside.fits(place)))
                        .max_by_key(|place| beside.rank(side, place, on_screen));
                    if best.is_some() {
                        return best;
                    }
                }
            }
        }
        None
    }

    /// The groups within `radius` canvas pixels of `point`,
    /// nearest first.
    #[must_use]
    pub fn near(&self, point: (i32, i32), radius: u32) -> Vec<GroupId> {
        let reach = i64::from(radius).saturating_mul(i64::from(radius));
        let mut hits: Vec<(i64, GroupId)> = self
            .places
            .iter()
            .map(|(id, place)| (place.distance_sq(point), *id))
            .filter(|(distance, _)| *distance <= reach)
            .collect();
        hits.sort_unstable();
        hits.into_iter().map(|(_, id)| id).collect()
    }

    /// The groups a selection rectangle touches, in id order.
    #[must_use]
    pub fn within(&self, area: CanvasPlace) -> Vec<GroupId> {
        self.places
            .iter()
            .filter(|(_, place)| place.intersects(&area))
            .map(|(id, _)| *id)
            .collect()
    }

    /// The smallest place holding every one of `groups` that is placed.
    #[must_use]
    pub fn bounds_of(&self, groups: &[GroupId]) -> Option<CanvasPlace> {
        let placed: Vec<CanvasPlace> = groups.iter().filter_map(|g| self.place_of(*g)).collect();
        let x0 = placed.iter().map(|p| i64::from(p.x)).min()?;
        let y0 = placed.iter().map(|p| i64::from(p.y)).min()?;
        let x1 = placed.iter().map(CanvasPlace::right).max()?;
        let y1 = placed.iter().map(CanvasPlace::bottom).max()?;
        Some(CanvasPlace::new(
            i32::try_from(x0).ok()?,
            i32::try_from(y0).ok()?,
            u32::try_from(x1.saturating_sub(x0)).ok()?,
            u32::try_from(y1.saturating_sub(y0)).ok()?,
        ))
    }

    /// Forget the places of groups for which `live` is false: a closed
    /// window leaves its space free. Returns whether any went.
    pub fn retain_groups(&mut self, live: impl Fn(GroupId) -> bool) -> bool {
        let held = self.places.len().saturating_add(self.floats.len());
        self.places.retain(|id, _| live(*id));
        self.floats.retain(|id, _| live(*id));
        self.places.len().saturating_add(self.floats.len()) != held
    }

    /// Stand every group `strips` hold that has no place yet, and keep every place that
    /// is there: the framed canvas, the spaces canvas and the traditional desktop draw a
    /// group by its place, so a window the shell opened and nobody placed stands on none of
    /// them, and a layout a person opened an app on looks empty.
    ///
    /// The groups of a strip stand in a row from the strip's corner (`canvas.md` §6), so the
    /// next one goes just past the last one that stands; a strip whose windows none stand yet
    /// starts at the origin, where the first place a person types at also lands. The point an
    /// absent group belongs to is the free space nearest its strip's row, so a window the
    /// person opened is where they look for it. Returns whether any was placed.
    pub fn place_missing(&mut self, strips: &[Strip]) -> bool {
        let gap = i64::from(CANVAS_GAP);
        let step = i64::from(MISSING_PLACE_WIDTH).saturating_add(gap);
        // Where each group that does not stand yet belongs, walking each strip's row from
        // its corner: a strip starts at the origin and each group of it follows the one
        // before, at its right edge, whether that one stands already or is about to.
        let mut wanted: Vec<(GroupId, (i32, i32))> = Vec::new();
        for strip in strips {
            let mut x = 0_i64;
            for group in &strip.window_groups {
                if let Some(place) = self.place_of(group.id) {
                    x = place.right().saturating_add(gap);
                    continue;
                }
                wanted.push((group.id, (clamp_point(x), 0)));
                x = x.saturating_add(step);
            }
        }
        for (group, anchor) in &wanted {
            let place =
                self.free_place_near(*anchor, MISSING_PLACE_WIDTH, MISSING_PLACE_HEIGHT, None);
            self.places.insert(*group, place);
        }
        !wanted.is_empty()
    }
}

impl crate::tree::Workspace {
    /// Every place stands for a window on the layout: one that left it takes its place and its float
    /// with it, so the next window stands in the room it left instead of beside a hole. Called
    /// wherever a group can go — a close, a command, a layout read back from its record.
    pub fn forget_gone_places(&mut self) {
        let live: std::collections::BTreeSet<GroupId> = self
            .strips
            .iter()
            .flat_map(|strip| &strip.window_groups)
            .map(|group| group.id)
            .collect();
        self.layout.retain_groups(|group| live.contains(&group));
    }
}

/// The canvas places of a stored layout, each a window's size ([`CanvasPlace::window`]).
fn deserialize_window_places<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<GroupId, CanvasPlace>, D::Error> {
    let places: BTreeMap<GroupId, CanvasPlace> =
        crate::serde_ids::deserialize_id_btree_map(deserializer)?;
    Ok(places
        .into_iter()
        .map(|(group, place)| (group, place.window()))
        .collect())
}

/// A side of a turn an answer's view stands on, in the order they are tried
/// ([`WorkspaceLayout::free_place_within`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    Trailing,
    Below,
    Leading,
    Above,
}

/// The sizes an answer's view is tried at beside its turn, `asked` first: a room too small for it
/// takes the largest part of it that fits, no smaller than a window.
fn shrinking((width, height): (u32, u32)) -> impl Iterator<Item = (u32, u32)> {
    [(4, 4), (3, 4), (4, 3), (3, 3), (2, 3), (3, 2), (2, 2)]
        .into_iter()
        .map(move |(w, h)| {
            (
                (width.saturating_mul(w) / 4).max(WINDOW_MIN_WIDTH),
                (height.saturating_mul(h) / 4).max(WINDOW_MIN_HEIGHT),
            )
        })
}

/// A turn's place, and what the person sees once it is in view: what an answer's view is placed
/// against ([`WorkspaceLayout::free_place_within`]). Canvas pixels, edges exclusive.
struct Beside {
    turn: CanvasPlace,
    /// The view the camera shows the turn in: what the person saw, panned the least that shows
    /// the turn whole with a gap at each edge.
    view: (i64, i64, i64, i64),
    /// How much the screen holds with a gap clear of each edge, its width then its height.
    span: (i64, i64),
}

impl Beside {
    fn new(turn: CanvasPlace, seen: CanvasPlace) -> Self {
        let gap = i64::from(CANVAS_GAP);
        let (x0, y0, x1, y1) = on_screen(&turn);
        let (width, height) = (i64::from(seen.width), i64::from(seen.height));
        // The least pan along one axis that holds `low..high`; one longer than the view shows its
        // start, as the camera does.
        let along = |start: i64, length: i64, low: i64, high: i64| {
            let (low, high) = (low.saturating_sub(gap), high.saturating_add(gap));
            if high.saturating_sub(low) > length || low < start {
                low
            } else if high > start.saturating_add(length) {
                high.saturating_sub(length)
            } else {
                start
            }
        };
        let x = along(i64::from(seen.x), width, x0, x1);
        let y = along(i64::from(seen.y), height, y0, y1);
        Self {
            turn,
            view: (x, y, x.saturating_add(width), y.saturating_add(height)),
            span: (
                width.saturating_sub(gap.saturating_mul(2)),
                height.saturating_sub(gap.saturating_mul(2)),
            ),
        }
    }

    /// The most an answer's view of `asked` size takes on `side`: `asked` itself, or with
    /// `on_screen` the part of it that stands on the screen beside the turn. `None` when not even
    /// a window's least size does.
    fn size(&self, side: Side, (width, height): (u32, u32), on_screen: bool) -> Option<(u32, u32)> {
        if !on_screen {
            return Some((width, height));
        }
        let (band, gap, row_gap) = (
            i64::from(CANVAS_BAND),
            i64::from(CANVAS_GAP),
            i64::from(CANVAS_ROW_GAP),
        );
        let (turn_width, turn_height) = (i64::from(self.turn.width), i64::from(self.turn.height));
        let (most_width, most_height) = match side {
            Side::Trailing | Side::Leading => (
                self.span.0.saturating_sub(turn_width).saturating_sub(gap),
                self.span.1.saturating_sub(band),
            ),
            Side::Below | Side::Above => (
                self.span.0,
                self.span
                    .1
                    .saturating_sub(band)
                    .saturating_sub(turn_height)
                    .saturating_sub(row_gap),
            ),
        };
        let width = i64::from(width).min(most_width);
        let height = i64::from(height).min(most_height);
        let least = width >= i64::from(WINDOW_MIN_WIDTH) && height >= i64::from(WINDOW_MIN_HEIGHT);
        least.then(|| {
            (
                u32::try_from(width).unwrap_or(WINDOW_MIN_WIDTH),
                u32::try_from(height).unwrap_or(WINDOW_MIN_HEIGHT),
            )
        })
    }

    /// The spots of `width` × `height` adjacent to the turn on `side`: against that edge
    /// and sharing some of its length, lined up with the turn's ends, the view's edges and each
    /// place's edges, where the free spots nearest each lie.
    fn spots<'a>(
        &'a self,
        side: Side,
        (width, height): (u32, u32),
        others: &'a [CanvasPlace],
    ) -> impl Iterator<Item = CanvasPlace> + 'a {
        let (gap, row_gap, band) = (
            i64::from(CANVAS_GAP),
            i64::from(CANVAS_ROW_GAP),
            i64::from(CANVAS_BAND),
        );
        let (w, h) = (i64::from(width), i64::from(height));
        let turn = self.turn;
        let (view_left, view_top, view_right, view_bottom) = self.view;
        let across = matches!(side, Side::Trailing | Side::Leading);
        // Where it stands against the edge, and where along the edge it may slide.
        let at = match side {
            Side::Trailing => turn.right().saturating_add(gap),
            Side::Leading => i64::from(turn.x).saturating_sub(gap).saturating_sub(w),
            Side::Below => turn.bottom().saturating_add(row_gap),
            Side::Above => i64::from(turn.y).saturating_sub(row_gap).saturating_sub(h),
        };
        let (start, end, length) = if across {
            (i64::from(turn.y), turn.bottom(), h)
        } else {
            (i64::from(turn.x), turn.right(), w)
        };
        let mut along = if across {
            vec![
                start,
                end.saturating_sub(length),
                view_top.saturating_add(band),
                view_bottom.saturating_sub(length),
            ]
        } else {
            vec![
                start,
                end.saturating_sub(length),
                view_left,
                view_right.saturating_sub(length),
            ]
        };
        for o in others {
            along.extend(if across {
                [
                    o.bottom().saturating_add(row_gap),
                    i64::from(o.y)
                        .saturating_sub(row_gap)
                        .saturating_sub(length),
                ]
            } else {
                [
                    o.right().saturating_add(gap),
                    i64::from(o.x).saturating_sub(gap).saturating_sub(length),
                ]
            });
        }
        along
            .into_iter()
            .filter(move |slide| *slide < end && slide.saturating_add(length) > start)
            .map(move |slide| {
                let (x, y) = if across { (at, slide) } else { (slide, at) };
                CanvasPlace::new(clamp_point(x), clamp_point(y), width, height)
            })
    }

    /// Whether `place` stands whole in the view the turn is seen in, clear of its edges: shown
    /// there, nothing pans.
    fn in_view(&self, place: &CanvasPlace) -> bool {
        let gap = i64::from(CANVAS_GAP);
        let (x0, y0, x1, y1) = on_screen(place);
        let (vx0, vy0, vx1, vy1) = self.view;
        x0 >= vx0.saturating_add(gap)
            && y0 >= vy0.saturating_add(gap)
            && x1 <= vx1.saturating_sub(gap)
            && y1 <= vy1.saturating_sub(gap)
    }

    /// Whether `place` and the turn stand on the screen together.
    fn fits(&self, place: &CanvasPlace) -> bool {
        let (ax0, ay0, ax1, ay1) = on_screen(&self.turn);
        let (bx0, by0, bx1, by1) = on_screen(place);
        ax1.max(bx1).saturating_sub(ax0.min(bx0)) <= self.span.0
            && ay1.max(by1).saturating_sub(ay0.min(by0)) <= self.span.1
    }

    /// How `place` ranks among the spots on `side`, higher first: how much of it the person sees
    /// with the turn in view, then how near the turn's own line it stands, then the topmost and
    /// leftmost, so the same canvas gives the same spot on every device. Spots that stand on the
    /// screen together with the turn (`on_screen`) are all seen whole once the camera pans to
    /// both, so only the turn's line ranks them.
    fn rank(
        &self,
        side: Side,
        place: &CanvasPlace,
        on_screen_with_turn: bool,
    ) -> (i64, i64, i64, i64) {
        let (x0, y0, x1, y1) = on_screen(place);
        let (vx0, vy0, vx1, vy1) = self.view;
        let shown = if on_screen_with_turn {
            0
        } else {
            x1.min(vx1)
                .saturating_sub(x0.max(vx0))
                .max(0)
                .saturating_mul(y1.min(vy1).saturating_sub(y0.max(vy0)).max(0))
        };
        let off_line = match side {
            Side::Trailing | Side::Leading => {
                i64::from(place.y).saturating_sub(i64::from(self.turn.y))
            }
            Side::Below | Side::Above => i64::from(place.x).saturating_sub(i64::from(self.turn.x)),
        };
        (
            shown,
            off_line.saturating_abs().saturating_neg(),
            i64::from(place.y).saturating_neg(),
            i64::from(place.x).saturating_neg(),
        )
    }
}

/// What `place` takes on the screen: its view and the band over it, as its leading, top,
/// trailing and bottom edges.
fn on_screen(place: &CanvasPlace) -> (i64, i64, i64, i64) {
    (
        i64::from(place.x),
        i64::from(place.y).saturating_sub(i64::from(CANVAS_BAND)),
        place.right(),
        place.bottom(),
    )
}

/// A canvas coordinate as a place keeps it.
fn clamp_point(value: i64) -> i32 {
    i32::try_from(value).unwrap_or(if value < 0 { i32::MIN } else { i32::MAX })
}

#[cfg(test)]
mod workspace_layout_tests {
    use super::*;

    fn canvas(places: &[(GroupId, CanvasPlace)]) -> WorkspaceLayout {
        WorkspaceLayout {
            kind: LayoutKind::Canvas,
            places: places.iter().copied().collect(),
            floats: BTreeMap::new(),
        }
    }

    tilo_test_env::scenario! {
        fn a_free_place_is_found_near_beside_and_next_to_windows;

        phase an_empty_canvas_places_a_window_exactly_where_pointed;
        {
            let place = WorkspaceLayout::default().free_place_near((120, -40), 640, 480, None);
            assert_eq!(place, CanvasPlace::new(120, -40, 640, 480));
        }

        phase a_window_in_the_way_is_never_moved_and_never_covered;
        {
            let board = CanvasPlace::new(0, 0, 800, 600);
            let layout = canvas(&[(1, board)]);
            let place = layout.free_place_near((100, 100), 640, 480, None);
            assert!(
                !place.crowds(&board, CANVAS_GAP),
                "{place:?} crowds {board:?}"
            );
            assert_eq!(layout.place_of(1), Some(board), "nothing moved");
        }

        phase the_free_place_chosen_is_the_nearest_one;
        {
            // A wide window above the anchor: going below it is nearer than
            // going past its far side.
            let wide = CanvasPlace::new(-1000, 0, 2000, 300);
            let layout = canvas(&[(1, wide)]);
            let place = layout.free_place_near((0, 100), 400, 300, None);
            assert_eq!((place.x, place.y), (0, 368));
        }

        phase placing_is_deterministic_and_fills_around_a_crowd;
        {
            let mut layout = canvas(&[]);
            let mut placed = Vec::new();
            for id in 1..=6 {
                let place = layout.free_place_near((0, 0), 400, 300, None);
                for other in &placed {
                    assert!(
                        !place.crowds(other, CANVAS_GAP),
                        "{place:?} crowds {other:?}"
                    );
                }
                assert_eq!(place, layout.free_place_near((0, 0), 400, 300, None));
                layout.places.insert(id, place);
                placed.push(place);
            }
        }

        phase beside_a_window_means_its_trailing_side_when_free;
        {
            let card = CanvasPlace::new(0, 0, 400, 300);
            let layout = canvas(&[(1, card)]);
            let place = layout.free_place_beside(card, 300, 200, None);
            assert_eq!((place.x, place.y), (424, 0));
        }

        phase next_to_a_window_walks_right_then_down_and_never_above_it;
        {
            // The conversation at the origin: the first place opened beside it stands at its right on
            // its own line, the second below it, the third below its right — and none above it or to
            // its left, where a person would not look for what they asked for.
            let conversation = CanvasPlace::new(0, 0, 720, 520);
            let mut layout = canvas(&[(1, conversation)]);
            let opened: Vec<CanvasPlace> = (2..=7)
                .map(|id| {
                    let place = layout.free_place_next_to(conversation, 720, 520, None);
                    assert!(!place.crowds(&conversation, CANVAS_GAP), "{place:?}");
                    assert!(
                        place.y >= conversation.y,
                        "never above the anchor: {place:?}"
                    );
                    assert!(
                        place.x >= conversation.x,
                        "never to the anchor's leading side: {place:?}"
                    );
                    for other in &opened_places(&layout) {
                        assert!(
                            !place.crowds(other, CANVAS_GAP),
                            "{place:?} crowds {other:?}"
                        );
                    }
                    layout.places.insert(id, place);
                    place
                })
                .collect();
            assert_eq!(
                (opened[0].x, opened[0].y),
                (744, 0),
                "the first stands beside the conversation, at its line"
            );
            assert_eq!((opened[1].x, opened[1].y), (0, 588), "the second below it");
            assert_eq!(
                (opened[2].x, opened[2].y),
                (744, 588),
                "the third under its right"
            );
            let places: Vec<CanvasPlace> = opened_places(&layout);
            assert_eq!(places.len(), 7, "every place stands clear of the rest");
        }

        phase a_window_being_placed_again_ignores_its_own_old_place;
        {
            let card = CanvasPlace::new(0, 0, 400, 300);
            let layout = canvas(&[(1, card)]);
            assert_eq!(
                layout.free_place_near((10, 10), 400, 300, Some(1)).x,
                10
            );
        }
    }

    /// A hugging place takes on the canvas the height it is drawn at once its canvas reports one,
    /// and its full height until then: the free area under it is an area a person can use, though
    /// the place could grow into it, and a place that does not hug takes its full height whatever
    /// a drawing says (canvas.md §1, §7). A place sized by hand stops hugging and drops the height
    /// it was drawn at.
    #[test]
    fn a_hugging_place_leaves_the_area_below_it_free_where_it_is_drawn() {
        // A hugging place: 720 × 520 is the most it takes, drawn 140 tall. A place dragged just
        // under where it is drawn, at the row gap's clearance, is free there.
        let hugging = CanvasPlace::new(0, 0, 720, 520).hugging();
        let mut drawn = hugging;
        assert!(drawn.drawn(140), "a hugging place keeps its drawn height");
        assert_eq!(
            drawn.occupied().height,
            140,
            "it takes what it is drawn at on the canvas"
        );
        // A place dragged to just under what is drawn, at the row gap's clearance.
        let below_drawn = i32::try_from(140 + CANVAS_ROW_GAP).expect("a canvas row");
        let under = CanvasPlace::new(0, below_drawn + 1, 400, 260);
        assert!(
            !under.crowds_on_canvas(&drawn),
            "the area below the height it is drawn at is free: {under:?} vs {drawn:?}"
        );
        assert!(
            under.crowds_on_canvas(&hugging),
            "without a drawn height, the place takes its full height: {hugging:?}"
        );
        let layout = canvas(&[(1, drawn)]);
        let placed = layout.free_place_near((0, below_drawn), 400, 260, None);
        assert_eq!(
            (placed.x, placed.y),
            (0, below_drawn),
            "a place put under it stands where it was asked, clear of what is drawn"
        );
        // The area to its trailing side is the line it is drawn to, not its tallest: a place put
        // beside it at its line stands there.
        let beside = canvas(&[(1, drawn)]).free_place_next_to(drawn, 300, 200, None);
        let trailing = i32::try_from(720 + CANVAS_GAP).expect("a canvas column");
        assert_eq!(
            (beside.x, beside.y),
            (trailing, 0),
            "beside the place at its line, clear of what is drawn"
        );
        assert!(!beside.crowds_on_canvas(&drawn));
        // A place that does not hug keeps its full height, whatever a drawing reported.
        let mut sized = CanvasPlace::new(0, 0, 720, 520);
        assert!(!sized.drawn(140), "a place that does not hug keeps nothing");
        assert_eq!(sized.drawn_height, None);
        let below = CanvasPlace::new(0, 300, 400, 260);
        assert!(
            below.crowds_on_canvas(&sized),
            "a place its person sized takes its full height: {sized:?}"
        );
        // Sized by hand, a hugging place stops hugging and keeps the size it was given.
        let mut by_hand = canvas(&[(1, hugging)]);
        let group: GroupId = 1;
        by_hand.set_drawn_height(group, 140);
        assert_eq!(
            by_hand.place_of(group).and_then(|p| p.drawn_height),
            Some(140)
        );
        assert!(by_hand.set_size(group, 700, 500));
        let resized = by_hand.place_of(group).expect("its place");
        assert!(!resized.hugs, "sized by hand it no longer hugs");
        assert_eq!(resized.drawn_height, None, "and the drawing is dropped");
        assert_eq!((resized.width, resized.height), (700, 500));
    }

    /// A place under another keeps its caption's band clear of the one above (canvas.md §7,
    /// "Captions never hidden"): one over the other they stand the band and the gap apart, side by
    /// side the gap.
    #[test]
    fn a_place_below_another_keeps_its_caption_clear() {
        let above = CanvasPlace::new(0, 0, 400, 300);
        let next = canvas(&[(1, above)]).free_place_next_to(above, 400, 300, None);
        assert_eq!((next.x, next.y), (424, 0), "side by side the gap is enough");
        let crowded = canvas(&[(1, above), (2, CanvasPlace::new(424, 0, 400, 300))]);
        let below = crowded.free_place_next_to(above, 400, 300, None);
        assert_eq!(
            (below.x, below.y),
            (0, 300 + 44 + 24),
            "below, the band and the gap stand between them"
        );
        let near = canvas(&[(1, above)]).free_place_near((0, 310), 400, 300, None);
        assert!(
            !near.crowds_on_canvas(&above),
            "{near:?} keeps its caption clear of {above:?}"
        );
        assert!(
            CanvasPlace::new(0, 330, 400, 300).crowds_on_canvas(&above),
            "a place whose band stands under the one above crowds it"
        );
    }

    /// Whether `a` and `b`, with the bands over them, stand together on a screen of `seen`'s size.
    fn on_one_screen(a: CanvasPlace, b: CanvasPlace, seen: CanvasPlace) -> bool {
        let band = i64::from(CANVAS_BAND);
        let left = i64::from(a.x.min(b.x));
        let top = i64::from(a.y.min(b.y)).saturating_sub(band);
        a.right().max(b.right()).saturating_sub(left) <= i64::from(seen.width)
            && a.bottom().max(b.bottom()).saturating_sub(top) <= i64::from(seen.height)
    }

    /// Beside a turn, an answer's view stands in the free spot adjacent to it, trailing side
    /// first, at the turn's line, in what the person sees when a side has a free spot there: a turn near
    /// the trailing edge has it stand on another side in view rather than past the edge. Only
    /// when no side has a free spot in view does it stand past the edge, made smaller so the turn and it
    /// stand on the screen together (the camera pans to both). Nothing adjacent free: nothing.
    #[test]
    fn an_answer_stands_adjacent_to_its_turn() {
        let seen = CanvasPlace::new(0, 0, 1280, 740);
        let turn = CanvasPlace::new(100, 100, 520, 480);
        let place = canvas(&[(1, turn)])
            .free_place_within(turn, (600, 400), seen, None)
            .expect("a spot beside the turn");
        assert_eq!(
            (place.x, place.y, place.width, place.height),
            (644, 100, 600, 400),
            "the trailing side, at its line, at its size"
        );
        assert!(place.inside(&seen) && !place.crowds_on_canvas(&turn));
        // The turn near the trailing edge, with a free spot on its leading side: the answer stands
        // there, in view, rather than past the edge with the camera panning away from what the
        // person was looking at.
        let right = CanvasPlace::new(700, 100, 520, 480);
        let place = canvas(&[(1, right)])
            .free_place_within(right, (600, 400), seen, None)
            .expect("room beside the turn");
        assert!(place.inside(&seen), "in view: {place:?}");
        assert!(
            place.right() <= i64::from(right.x),
            "on the leading side: {place:?}"
        );
        assert!(!place.crowds_on_canvas(&right));
        // No side with a free spot in view (the turn fills the view's height and the leading side is
        // taken): the trailing side past the edge, the two on one screen for the camera to show.
        let tall = CanvasPlace::new(700, 30, 520, 680);
        let taken = CanvasPlace::new(0, 0, 676, 740);
        let place = canvas(&[(1, tall), (2, taken)])
            .free_place_within(tall, (600, 400), seen, None)
            .expect("room on the trailing side");
        assert_eq!((place.x, place.y), (1244, 30), "trailing, at its line");
        assert!(on_one_screen(tall, place, seen), "{place:?} with {tall:?}");
        // A turn too wide for the whole size beside it on one screen: a smaller part of it.
        let wide = CanvasPlace::new(0, 60, 700, 480);
        let place = canvas(&[(1, wide)])
            .free_place_within(wide, (720, 520), seen, None)
            .expect("a smaller room beside it");
        assert_eq!((place.x, place.y), (724, 60), "trailing, at its line");
        assert!(place.width < 720, "made smaller to fit: {place:?}");
        assert!(on_one_screen(wide, place, seen), "{place:?} with {wide:?}");
        // Every side taken: the caller stands it further along.
        let small = CanvasPlace::new(0, 0, 400, 300);
        let ring = canvas(&[
            (1, small),
            (2, CanvasPlace::new(-5000, -5000, 4976, 10000)),
            (3, CanvasPlace::new(448, -5000, 5000, 10000)),
            (4, CanvasPlace::new(-5000, -5000, 10000, 4932)),
            (5, CanvasPlace::new(-5000, 400, 10000, 5000)),
        ]);
        assert_eq!(ring.free_place_within(small, (720, 520), seen, None), None);
    }

    /// A turn asked by a click near the bottom of what the person sees, after another turn and
    /// its answer took the top of the view: the answer stands beside the turn at its line, never
    /// in the far corner of the view by the first turn, and the turn and it fit on one screen.
    #[test]
    fn an_answer_to_a_turn_near_the_bottom_of_the_view_stands_beside_it() {
        let seen = CanvasPlace::new(0, 0, 1280, 760);
        let first = CanvasPlace::new(26, 38, 358, 240);
        let notes = CanvasPlace::new(408, 44, 720, 520);
        let turn = CanvasPlace::new(640, 662, 520, 480);
        let place = canvas(&[(1, first), (2, notes), (3, turn)])
            .free_place_within(turn, (720, 520), seen, None)
            .expect("room beside the turn");
        let gap = i64::from(CANVAS_GAP);
        let trailing = i64::from(place.x) == turn.right().saturating_add(gap);
        let leading = place.right().saturating_add(gap) == i64::from(turn.x);
        assert!(
            (trailing || leading) && place.y == turn.y,
            "beside the turn, at its line: {place:?}"
        );
        assert!(on_one_screen(turn, place, seen), "{place:?} with {turn:?}");
        for other in [first, notes, turn] {
            assert!(
                !place.crowds_on_canvas(&other),
                "{place:?} clear of {other:?}"
            );
        }
    }

    /// "Show me my mails" asked at the top right of the view, with the screen below empty: the
    /// inbox stands below the turn, in view, never past the trailing edge with the camera
    /// panning the earlier turns off the screen.
    #[test]
    fn an_answer_takes_a_spot_in_view_before_one_past_the_edge() {
        let seen = CanvasPlace::new(0, 0, 1440, 860);
        let earlier = CanvasPlace::new(208, 173, 362, 100);
        let turn = CanvasPlace::new(625, 60, 520, 140);
        let place = canvas(&[(1, earlier), (2, turn)])
            .free_place_within(turn, (720, 520), seen, None)
            .expect("a spot beside the turn");
        assert!(place.inside(&seen), "in view: {place:?}");
        assert!(
            i64::from(place.y) >= turn.bottom(),
            "below the turn: {place:?}"
        );
        for other in [earlier, turn] {
            assert!(
                !place.crowds_on_canvas(&other),
                "{place:?} clear of {other:?}"
            );
        }
    }

    /// A window put at a click left of a place stands at the click, narrowed to the room before
    /// that place; free room keeps its width; a click with not even a window's least width free
    /// to its right has none, and a place below the click's reach is no obstacle.
    #[test]
    fn a_window_at_a_click_is_as_wide_as_the_free_room_to_its_right() {
        let inbox = CanvasPlace::new(900, 0, 1000, 1200);
        let layout = canvas(&[(1, inbox)]);
        assert_eq!(
            layout.free_width_at((300, 600), 720, 520),
            Some(900 - 24 - 300),
            "narrowed to the room before the inbox"
        );
        let width = layout.free_width_at((300, 600), 720, 520).expect("room");
        assert!(!CanvasPlace::new(300, 600, width, 520).crowds_on_canvas(&inbox));
        assert_eq!(
            layout.free_width_at((-1000, 600), 720, 520),
            Some(720),
            "room for all of it"
        );
        assert_eq!(
            layout.free_width_at((800, 600), 720, 520),
            None,
            "less than a window's least width before the inbox"
        );
        assert_eq!(
            layout.free_width_at((1000, 600), 720, 520),
            None,
            "inside the inbox"
        );
        assert_eq!(
            layout.free_width_at((300, 1400), 720, 520),
            Some(720),
            "below the inbox, clear of it"
        );
    }

    /// The places in `layout`, for saying one crowds none.
    fn opened_places(layout: &WorkspaceLayout) -> Vec<CanvasPlace> {
        layout.places.values().copied().collect()
    }

    tilo_test_env::scenario! {
        fn the_canvas_keeps_and_finds_its_places;

        phase nearby_and_selected_windows_are_found;
        {
            let layout = canvas(&[
                (1, CanvasPlace::new(0, 0, 100, 100)),
                (2, CanvasPlace::new(500, 0, 100, 100)),
                (3, CanvasPlace::new(2000, 2000, 100, 100)),
            ]);
            assert_eq!(layout.near((150, 50), 600), vec![1, 2]);
            assert_eq!(
                layout.within(CanvasPlace::new(50, 50, 500, 10)),
                vec![1, 2]
            );
            assert_eq!(
                layout.bounds_of(&[1, 2]),
                Some(CanvasPlace::new(0, 0, 600, 100))
            );
        }

        phase closing_a_window_frees_its_place;
        {
            let mut layout = canvas(&[(1, CanvasPlace::new(0, 0, 10, 10))]);
            assert!(layout.retain_groups(|id| id != 1));
            assert!(layout.places.is_empty());
        }

        phase a_place_let_go_elsewhere_on_the_canvas_keeps_its_size_and_moves_nothing_else;
        {
            // A drag that ended on the canvas: the place stands where it was let go, at the size it
            // had, and every other place stays exactly where it was.
            let mut layout = canvas(&[
                (1, CanvasPlace::new(0, 0, 400, 300)),
                (2, CanvasPlace::new(500, 0, 400, 300)),
            ]);
            assert!(layout.set_position(2, 900, 640));
            assert_eq!(
                layout.place_of(2),
                Some(CanvasPlace::new(900, 640, 400, 300)),
                "the place stands where it was let go, at its own size"
            );
            assert_eq!(
                layout.place_of(1),
                Some(CanvasPlace::new(0, 0, 400, 300)),
                "nothing else moved"
            );
            assert!(
                !layout.set_position(2, 900, 640),
                "a place already there is not a change"
            );
        }

        phase a_stored_workspace_keeps_its_canvas;
        {
            let mut workspace = crate::Workspace::new(crate::WorkspaceId::new(), "Work");
            workspace.layout = canvas(&[(920_001, CanvasPlace::new(40, -8, 400, 300))]);
            let json = serde_json::to_string(&workspace).expect("serialises");
            let back: crate::Workspace = serde_json::from_str(&json).expect("reads back");
            assert_eq!(back.layout, workspace.layout);
        }

        phase an_unknown_kind_reads_as_strips;
        {
            assert_eq!(LayoutKind::parse("canvas"), LayoutKind::Canvas);
            assert_eq!(LayoutKind::parse("spiral"), LayoutKind::Strips);
        }
    }

    tilo_test_env::scenario! {
        fn a_float_keeps_its_corner_and_its_size;

        phase a_window_is_never_kept_or_laid_out_a_sliver;
        // A window's place, on the canvas or floating, is never smaller than a window: an agent's
        // command or a record that asked for a sliver gets a window, and a sliver some older build
        // left still lays out a window wide.
        {
            let area = CanvasPlace::new(0, 32, 1280, 768);
            let sliver = FloatPlace {
                corner: FloatCorner::TopLeft,
                dx: 24,
                dy: 0,
                width: 2,
                height: 2,
            };
            assert_eq!(
                sliver.resolved(area),
                CanvasPlace::new(24, 32, WINDOW_MIN_WIDTH, WINDOW_MIN_HEIGHT),
                "laid out from the corner it keeps, a window wide"
            );
            let made = FloatPlace::new(FloatCorner::TopLeft, 24, 0, 2, 2);
            assert_eq!(
                (made.width, made.height),
                (WINDOW_MIN_WIDTH, WINDOW_MIN_HEIGHT)
            );
            let read: FloatPlace =
                serde_json::from_str(r#"{"corner":"top_left","dx":24,"dy":0,"width":2,"height":2}"#)
                    .unwrap();
            assert_eq!(read, made, "a stored sliver reads back a window");
            let mut layout = WorkspaceLayout::default();
            layout.set_float(7, Some(sliver));
            assert_eq!(layout.float_of(7), Some(made));
            // On a work area smaller than a window, the window fills the area.
            assert_eq!(
                made.resolved(CanvasPlace::new(0, 0, 100, 80)),
                CanvasPlace::new(0, 0, 100, 80)
            );

            let stored = r#"{"kind":"Canvas","places":{"7":{"x":-40,"y":12,"width":2,"height":0}}}"#;
            let layout: WorkspaceLayout = serde_json::from_str(stored).unwrap();
            assert_eq!(
                layout.place_of(7),
                Some(CanvasPlace::new(
                    -40,
                    12,
                    WINDOW_MIN_WIDTH,
                    WINDOW_MIN_HEIGHT
                ))
            );
            assert!(
                (f64::from(WINDOW_MIN_WIDTH) - crate::width::WIDTH_FLOOR_PX).abs() < f64::EPSILON,
                "a float or a canvas window is as narrow as a strip's window can get, no narrower"
            );
        }

        phase a_workspace_stored_before_floats_reads_with_none;
        {
            let json = r#"{"kind":"Strips","places":{}}"#;
            let layout: WorkspaceLayout = serde_json::from_str(json).expect("reads back");
            assert!(layout.floats.is_empty());
        }

        phase a_float_is_kept_from_its_corner_and_resolves_on_any_screen;
        {
            let float = FloatPlace::new(FloatCorner::BottomRight, 40, 24, 640, 480);
            let small = CanvasPlace::new(1280, 0, 1280, 720);
            assert_eq!(float.resolved(small), CanvasPlace::new(1880, 216, 640, 480));
            let large = CanvasPlace::new(0, 0, 2560, 1440);
            assert_eq!(float.resolved(large), CanvasPlace::new(1880, 936, 640, 480));
            // A window larger than the work area fills it rather than hanging off.
            let huge = FloatPlace::new(FloatCorner::TopLeft, 10, 10, 4000, 3000);
            assert_eq!(huge.resolved(small), CanvasPlace::new(1280, 0, 1280, 720));
            let back: FloatPlace =
                serde_json::from_str(&serde_json::to_string(&float).unwrap()).unwrap();
            assert_eq!(back, float);
        }

        phase dropping_a_window_anchors_it_to_the_corner_nearest_where_it_was_let_go;
        {
            let area = CanvasPlace::new(0, 0, 1600, 900);
            let bottom_right = FloatPlace::of_rect(area, CanvasPlace::new(900, 380, 640, 480));
            assert_eq!(bottom_right.corner, FloatCorner::BottomRight);
            assert_eq!((bottom_right.dx, bottom_right.dy), (60, 40));
            assert_eq!(
                bottom_right.resolved(area),
                CanvasPlace::new(900, 380, 640, 480),
                "the window stays exactly where it was let go"
            );
            let top_left = FloatPlace::of_rect(area, CanvasPlace::new(30, 20, 400, 300));
            assert_eq!(top_left.corner, FloatCorner::TopLeft);
            assert_eq!((top_left.dx, top_left.dy), (30, 20));
        }

        phase floating_and_snapping_back_moves_nothing_else;
        {
            let place = CanvasPlace::new(40, 40, 400, 300);
            let mut layout = canvas(&[(1, place)]);
            let float = FloatPlace::new(FloatCorner::TopRight, 10, 10, 640, 480);
            assert!(layout.set_float(1, Some(float)));
            assert!(!layout.set_float(1, Some(float)), "nothing changed");
            assert_eq!(layout.float_of(1), Some(float));
            assert_eq!(
                layout.place_of(1),
                Some(place),
                "its canvas place is kept"
            );
            assert!(layout.set_float(1, None));
            assert!(!layout.set_float(1, None));
            assert_eq!(layout.place_of(1), Some(place), "snap back is exact");
        }
    }
}
#[cfg(test)]
mod tab_name_tests {
    use super::*;

    fn terminal(title: &str) -> Tab {
        Tab {
            id: 7,
            title: title.into(),
            tile: TileContent::Foreign {
                app_id: "Alacritty".into(),
                surface_id: 3,
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        }
    }

    tilo_test_env::scenario! {
        fn a_tabs_name_and_its_windows_title;

        phase a_name_replaces_the_shown_title_and_keeps_the_windows_own;
        {
            let mut tab = terminal("~/src");
            assert!(tab.rename("  Build  "));
            assert_eq!(tab.title, "Build");
            assert_eq!(tab.app_title.as_deref(), Some("~/src"));
            assert!(tab.is_named());
            assert!(tab.rename("Deploy"));
            assert_eq!(tab.title, "Deploy");
            assert_eq!(tab.app_title.as_deref(), Some("~/src"));
            assert!(!tab.rename("Deploy"), "the same name again is no change");
        }

        phase a_window_title_while_named_updates_only_the_windows_own;
        {
            let mut tab = terminal("~/src");
            tab.rename("Build");
            assert!(tab.set_window_title("cargo build"));
            assert_eq!(tab.title, "Build");
            assert_eq!(tab.app_title.as_deref(), Some("cargo build"));
        }

        phase clearing_the_name_shows_the_latest_window_title;
        {
            let mut tab = terminal("~/src");
            tab.rename("Build");
            tab.set_window_title("cargo test");
            assert!(tab.rename("   "));
            assert_eq!(tab.title, "cargo test");
            assert_eq!(tab.app_title, None);
            assert!(!tab.is_named());
            assert!(!tab.rename(""), "a tab with no name has none to clear");
            assert!(tab.set_window_title("~"));
            assert_eq!(tab.title, "~");
        }

        phase the_same_window_title_again_is_no_change;
        {
            let mut tab = terminal("~/src");
            assert!(!tab.set_window_title("~/src"));
            tab.rename("Build");
            tab.set_window_title("vim");
            assert!(!tab.set_window_title("vim"));
            assert_eq!(tab.title, "Build");
        }

        phase a_window_that_names_nothing_keeps_the_name_it_has;
        {
            let mut tab = terminal("Notes");
            assert!(!tab.set_window_title(""));
            assert_eq!(tab.title, "Notes");
            assert!(!tab.set_window_title("   "), "blank is blank");
            assert_eq!(tab.title, "Notes");
            // Also while a person's name stands: their name stays, and the
            // window's own name it goes back to is the last one it gave.
            tab.rename("Read later");
            tab.set_window_title("Garden plan");
            assert!(!tab.set_window_title(""));
            assert!(tab.rename(""));
            assert_eq!(tab.title, "Garden plan");
        }

        phase a_tab_without_a_name_is_stored_as_before;
        {
            let json = serde_json::to_value(terminal("~/src")).expect("serialises");
            assert!(json.get("app_title").is_none());
            let old: Tab = serde_json::from_value(serde_json::json!({
                "id": 7,
                "title": "~/src",
                "tile": { "Foreign": { "app_id": "Alacritty", "surface_id": 3 } },
            }))
            .expect("a tab stored before names reads back");
            assert_eq!(old.app_title, None);
            let mut named = terminal("~/src");
            named.rename("Build");
            let back: Tab = serde_json::from_value(serde_json::to_value(&named).expect("serialises"))
                .expect("reads back");
            assert_eq!(back.title, "Build");
            assert_eq!(back.app_title.as_deref(), Some("~/src"));
        }
    }
}

#[cfg(test)]
mod tile_record_tests {
    use super::*;

    tilo_test_env::scenario! {
        fn tiles_read_back_as_written;

        phase every_tile_reads_back_as_written;
        {
            let tiles = [
                TileContent::Stub {
                    kind: AppKind::Terminal,
                    label: "Shell".into(),
                },
                TileContent::Foreign {
                    app_id: "Alacritty".into(),
                    surface_id: 3,
                },
                TileContent::LocalApp {
                    app_id: "so.tilo.notes".into(),
                    label: "Notes".into(),
                    note: String::new(),
                },
                TileContent::HostedWindow {
                    app_id: "xterm".into(),
                    label: "xterm".into(),
                    host: "ab12".into(),
                    host_label: "Work".into(),
                },
            ];
            for tile in tiles {
                let written = serde_json::to_string(&tile).unwrap();
                assert_eq!(
                    serde_json::from_str::<TileContent>(&written).unwrap(),
                    tile,
                    "{written}"
                );
            }
        }
    }
}
