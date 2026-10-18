//! `NavShell` — the one window shell.
//!
//! Every app with navigation is a `NavShell`: an optional sidebar of
//! destinations (HIG §1a), an optional overview list beside the content
//! (§1b), the content, and an optional inspector at the trailing edge. The
//! app hands it the columns and their headers; the shell owns everything
//! else, at the shell metrics (`theme.tokens.metrics`):
//!
//! - **One header row** across the top of the window holds each column's
//!   header (title, an optional count, a leading fill such as a path bar,
//!   and actions), so the row reads as one unified toolbar. Windows have no
//!   title bar; this row is the window's top edge. Its geometry is
//!   [`crate::chrome::HeaderRowFace`], the same face a painted app uses.
//!   Actions that do not fit first collapse to their icons, then move into
//!   a trailing More menu.
//! - **The sidebar toggle** is a Plain icon button (`Icon::PanelLeft`) the
//!   shell draws at the window's top-leading corner, on the sidebar while it
//!   shows and on the next column while it is hidden. One shortcut toggles
//!   it in every app (`HostShortcut::Sidebar`), and the button publishes the
//!   action ("Show sidebar" / "Hide sidebar") to the palette and to agents.
//! - **Back** is the shell's: a narrow window's push stack, and an app's own
//!   page stack ([`NavShell::with_back`]), share one `ChevronLeft` chip at
//!   the leading edge of the content's header.
//! - **History** is the window's: where the app has History, the host puts
//!   its control (`Icon::History`, a Plain icon button named "History") at
//!   the trailing edge of the row, and it reads as on while History shows.
//!   The app adds no History button of its own.
//! - **Responsive:** at any width the shell picks the layout in which
//!   everything shown is fully usable. The sidebar docks while it fits beside
//!   the content's real minimum (its [`ContentKind`]), shrinking from its
//!   width toward its own minimum first. Narrower, the window shows one
//!   column at a time: the sidebar (or overview) and the content become push
//!   screens with a Back button, and narrowing keeps the page the person was
//!   on. Only a content-primary app ([`ContentKind::Document`]) floats its
//!   sidebar as a drawer over the content instead. An inspector docks while
//!   the content keeps its minimum beside it and floats from the trailing
//!   edge otherwise. A drawer is never modal and dims nothing: the content
//!   beside it stays visible and live, and a press on it puts the drawer
//!   away. Each threshold has hysteresis (`metrics.layout_hysteresis`), so a
//!   window resized near it does not flicker. A person's show/hide choice is
//!   remembered per window size class. The narrowest window the shell works
//!   at is its compact layout ([`NavShell::minimum_width`]), which the host
//!   states to the window system.
//! - **Resizable:** dragging the sidebar's trailing edge resizes it;
//!   dragging it below its minimum width hides it.
//! - **Regions:** the sidebar stands on `Region::Sidebar`, the inspector on
//!   `Region::Inspector`, the overview and content on the window ground;
//!   each column's children draw with that region's theme.
//! - **Motion:** hide, show and push move with `motion.durations.base`;
//!   under reduced motion they cross-fade. A resize that switches the layout
//!   (docked ↔ one column at a time) moves the columns on from where they
//!   stood, so nothing jumps under the person during a live resize; under
//!   reduced motion the columns that moved fade in at their new place.
//!
//! There is no width, breakpoint, colour or padding to pass: the shell is
//! semantic. A filter field for the sidebar's list goes in
//! [`NavShell::with_filter`] (Ctrl+F focuses it).
//!
//! Slots and headers change in place (`set_content`, `set_content_header`,
//! …). A change made while an event is being routed takes effect when the
//! host next builds the tree, never in the middle of the event, so the host
//! never holds a widget the shell has dropped.

use crate::num::f32_of_f64;
use crate::num::f64_of_usize;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::LocalState;
use crate::chrome::HeaderRowMetrics;
use crate::icons::Icon;
use crate::local_state::LocalBinding;
use crate::platform::{HostShortcut, PlatformConventions};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::time::Instant;
use crate::widget::flap::{FlapState, SplitLayout};
use crate::widget::manual::{ManualContext, ManualInvalidation, ManualPolicy, MeasureConstraint};
use crate::widget::window_header::{HistoryControl, WindowHeader};
use crate::widget::{
    AppModel, Button, Element, ModelPath, SearchField, TaffyTreeExt, Widget, WidgetLayoutError,
};

mod header_parts;
use crate::widget::toast::{ToastKind, ToastStack};
use header_parts::{HeaderTitle, MoreActions};

/// What the sidebar becomes when the window is too narrow to dock it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NavMode {
    /// The first screen of a push stack: one column at a time fills the
    /// window, and choosing a destination pushes the content with a Back
    /// button. Every app's compact layout, unless its content is a
    /// [`ContentKind::Document`] (then the sidebar floats as a drawer).
    #[default]
    Push,
    /// Always docked: a compact icon rail (`NavList::rail`) that fits beside
    /// the content at every width.
    Docked,
}

/// What the shell's content is, so the shell knows the narrowest the
/// content still works at. The sidebar docks while it fits beside that
/// minimum; below it the window shows one column at a time. Apps say what
/// their content is, never how wide it is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ContentKind {
    /// A list or table of items: files, messages, tracks, events
    /// (`metrics.list_min_width`).
    List,
    /// A reading column: a message, an article, a forecast
    /// (`metrics.reading_min_width`).
    Reading,
    /// A page of settings, a form or a page of facts
    /// (`metrics.page_min_width`). The default.
    #[default]
    Page,
    /// A grid of media tiles: albums, photos, videos
    /// (`metrics.grid_min_width`).
    Grid,
    /// A document or editor the window is for, whose sidebar (an outline,
    /// the list of documents) is visited now and then: *content-primary*.
    /// Too narrow to dock, the sidebar floats as a drawer over the content
    /// instead of taking turns with it. Needs a reading column.
    Document,
}

impl ContentKind {
    /// The narrowest this kind of content still works at, from `metrics`
    /// (`theme.tokens.metrics`, or [`crate::tokens::Metrics::BASE`] at 1×).
    #[must_use]
    pub fn min_width(self, metrics: &crate::tokens::Metrics) -> f32 {
        match self {
            Self::List => metrics.list_min_width,
            Self::Reading | Self::Document => metrics.reading_min_width,
            Self::Page => metrics.page_min_width,
            Self::Grid => metrics.grid_min_width,
        }
    }

    /// Whether the content is the window's point and its sidebar an
    /// occasional visit: too narrow to dock, the sidebar floats over it.
    #[must_use]
    pub fn is_content_primary(self) -> bool {
        self == Self::Document
    }
}

/// A screen of the narrow push stack.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NavPane {
    /// The sidebar, as the first screen of a narrow window.
    Sidebar,
    /// The overview list.
    Overview,
    /// The content.
    #[default]
    Content,
}

/// Where the sidebar is this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SidebarPlacement {
    /// No sidebar.
    #[default]
    None,
    /// Beside the content (possibly hidden by the person).
    Docked,
    /// A floating drawer over the content.
    Drawer,
    /// The first screen of the push stack.
    Screen,
}

/// Where the inspector is this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InspectorPlacement {
    #[default]
    None,
    Docked,
    Drawer,
}

/// Where the overview is this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OverviewPlacement {
    /// No overview.
    #[default]
    None,
    /// The overview list, beside the content or as a push screen.
    List,
    /// Collapsed into its rail beside the content ([`NavShell::overview_rail`]).
    Rail,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Column {
    Sidebar,
    Overview,
    Content,
    Inspector,
}

impl Column {
    const ALL: [Self; 4] = [
        Self::Sidebar,
        Self::Overview,
        Self::Content,
        Self::Inspector,
    ];

    fn index(self) -> usize {
        match self {
            Self::Sidebar => 0,
            Self::Overview => 1,
            Self::Content => 2,
            Self::Inspector => 3,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Sidebar => "sidebar",
            Self::Overview => "overview",
            Self::Content => "content",
            Self::Inspector => "inspector",
        }
    }
}

/// `array`'s entry for `column`.
fn at<T>(array: &[T; 4], column: Column) -> &T {
    let [sidebar, overview, content, inspector] = array;
    match column {
        Column::Sidebar => sidebar,
        Column::Overview => overview,
        Column::Content => content,
        Column::Inspector => inspector,
    }
}

/// `array`'s entry for `column`, to change.
fn at_mut<T>(array: &mut [T; 4], column: Column) -> &mut T {
    let [sidebar, overview, content, inspector] = array;
    match column {
        Column::Sidebar => sidebar,
        Column::Overview => overview,
        Column::Content => content,
        Column::Inspector => inspector,
    }
}

/// One retained child of the shell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Part {
    Body(Column),
    /// The overview's rail, standing in for the overview where the list
    /// does not fit.
    Rail,
    /// A column's title (and count) in the header row.
    Title(Column),
    /// The fill between a header's leading buttons and its actions.
    Leading(Column),
    /// One of a header's actions.
    Action(Column, usize),
    /// The More button the actions overflow into.
    More(Column),
    /// Controls under the overview's header.
    Controls,
    Filter,
    Toggle,
    Back,
    InspectorToggle,
    /// The window's History control; `true` is the one shown while History
    /// is open.
    History(bool),
}

/// A column's header: a title the shell paints and actions the app owns.
struct Header {
    title: HeaderTitle,
    binding: Option<LocalBinding<String>>,
    actions: Vec<Element>,
    more: MoreActions,
    leading: Option<Element>,
}

impl Header {
    fn new(column: Column) -> Self {
        Self {
            title: HeaderTitle::new(),
            binding: None,
            actions: Vec::new(),
            more: MoreActions::new(&format!("nav-shell-more-{}", column.key())),
            leading: None,
        }
    }
}

/// What an optional slot becomes at the next build: the element, or none.
struct Staged(Option<Element>);

impl Staged {
    fn is_some(&self) -> bool {
        self.0.is_some()
    }
}

/// Slot and header changes waiting for the next build of the host tree.
#[derive(Default)]
struct Pending {
    bodies: [Option<Staged>; 4],
    actions: [Option<Vec<Element>>; 4],
    leading: [Option<Staged>; 4],
    controls: Option<Staged>,
    rail: Option<Staged>,
}

impl Pending {
    fn is_empty(&self) -> bool {
        self.bodies.iter().all(Option::is_none)
            && self.actions.iter().all(Option::is_none)
            && self.leading.iter().all(Option::is_none)
            && self.controls.is_none()
            && self.rail.is_none()
    }
}

/// The shell metrics resolved against the theme the tree was built with.
#[derive(Clone, Copy, Debug)]
struct Metrics {
    row: HeaderRowMetrics,
    header: f64,
    chip: f64,
    inset: f64,
    gap: f64,
    sidebar: f64,
    sidebar_min: f64,
    sidebar_max: f64,
    rail: f64,
    overview: f64,
    inspector: f64,
    drawer: f64,
    drawer_dismiss_band: f64,
    list_min: f64,
    reading_min: f64,
    page_min: f64,
    grid_min: f64,
    compact_min: f64,
    hysteresis: f64,
    filter: f64,
    resize_grip: f64,
    scale: f64,
}

impl Metrics {
    fn resolve(theme: &Theme) -> Self {
        let m = theme.tokens.metrics;
        let s = theme.tokens.spacing;
        let growth = f64::from(theme.text_growth());
        let row = HeaderRowMetrics::resolve(theme);
        Self {
            row,
            header: row.height,
            chip: row.chip,
            inset: row.inset,
            gap: row.gap,
            sidebar: f64::from(m.sidebar_width) * growth,
            sidebar_min: f64::from(m.sidebar_min_width) * growth,
            sidebar_max: f64::from(m.sidebar_max_width) * growth,
            rail: f64::from(m.rail_width),
            overview: f64::from(m.overview_width) * growth,
            inspector: f64::from(m.inspector_width) * growth,
            drawer: f64::from(m.drawer_width) * growth,
            drawer_dismiss_band: f64::from(m.min_pointer_target + s.s4),
            list_min: f64::from(m.list_min_width) * growth,
            reading_min: f64::from(m.reading_min_width) * growth,
            page_min: f64::from(m.page_min_width) * growth,
            grid_min: f64::from(m.grid_min_width) * growth,
            compact_min: f64::from(m.window_min_width) * growth,
            hysteresis: f64::from(m.layout_hysteresis),
            filter: crate::widget::text_control_height(
                f64::from(crate::tokens::Metrics::BASE.min_pointer_target),
                theme,
            ),
            resize_grip: f64::from(s.s1),
            scale: theme.responsive_scale(),
        }
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::resolve(&Theme::dark())
    }
}

/// How a column's actions fit its header, decided by the layout policy.
#[derive(Clone, Debug, Default)]
struct ColumnFit {
    /// Which actions are buttons that can show their icon alone.
    capable: Vec<bool>,
    /// Which of them show their icon alone right now.
    compact_now: Vec<bool>,
    /// Each action's width with its label, measured while it showed it.
    full: Vec<Option<f64>>,
    /// The labelled buttons collapse to their icons.
    compact: bool,
    /// How many actions stand in the row; the rest are in the More menu.
    /// `None` until the first layout decides.
    shown: Option<usize>,
}

/// Everything the geometry depends on. Shared with the layout policy the
/// host runs, so the policy and the shell compute the same rects.
#[allow(
    clippy::struct_excessive_bools,
    reason = "which slots the app filled are independent facts"
)]
#[derive(Clone, Debug)]
struct Inputs {
    mode: NavMode,
    kind: ContentKind,
    metrics: Metrics,
    has_sidebar: bool,
    has_overview: bool,
    has_rail: bool,
    has_inspector: bool,
    has_filter: bool,
    has_controls: bool,
    /// The window's History control, where the host gave the shell one.
    history: Option<HistoryControl>,
    /// Room for the window's controls at the trailing end of the row, while
    /// the host merged the window's bar into it (a maximized window).
    window_controls: Option<f64>,
    /// The app put its header in the window's bar
    /// ([`NavShell::with_header_in_window_bar`]).
    header_in_bar: bool,
    /// The person reached for the window's bar.
    bar_revealed: bool,
    /// How far down the window's top edge the bar stands: below the
    /// desktop's top bar while that is shown over the window.
    bar_top: f64,
    titled: [bool; 4],
    actions: [usize; 4],
    leading: [bool; 4],
    /// The app's own page stack can go back ([`NavShell::with_back`]).
    content_back: bool,
    /// The docked sidebar width a person dragged to, authored at 1×.
    user_sidebar: Option<f64>,
    sidebar: FlapState,
    inspector: FlapState,
    pane: NavPane,
    /// Eased position in the push stack: the front screen's index while
    /// settled, fractional while sliding.
    push_pos: Option<f64>,
    push_from: Option<f64>,
    push_progress: f64,
    /// The parts in the order they were built (their placement index).
    parts: Vec<Part>,
    /// How each column's actions fit, written by the layout policy.
    fit: Rc<RefCell<[ColumnFit; 4]>>,
    /// The layout the last applied geometry chose, for hysteresis.
    held: Held,
}

/// Which side of each threshold the last applied layout stood on (`None`
/// before the first layout). A layout that switched away switches back only
/// once the window is `metrics.hysteresis` past its threshold.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Held {
    sidebar_docked: Option<bool>,
    inspector_docked: Option<bool>,
    list_fits: Option<bool>,
    rail_fits: Option<bool>,
}

impl Held {
    fn of(g: &NavGeometry) -> Self {
        Self {
            sidebar_docked: Some(g.sidebar == SidebarPlacement::Docked),
            inspector_docked: Some(g.inspector == InspectorPlacement::Docked),
            list_fits: Some(g.overview == OverviewPlacement::List && g.side_by_side),
            // Wherever the list fits, its rail does too.
            rail_fits: Some(g.side_by_side),
        }
    }
}

/// Whether `room` reaches `threshold`, given the side the layout stood on
/// last: a layout that does not hold needs `band` more room to come back.
fn reaches(room: f64, threshold: f64, held: Option<bool>, band: f64) -> bool {
    let threshold = if held == Some(false) {
        threshold + band
    } else {
        threshold
    };
    room >= threshold - 1e-6
}

impl Default for Inputs {
    fn default() -> Self {
        Self {
            mode: NavMode::Push,
            kind: ContentKind::default(),
            metrics: Metrics::default(),
            has_sidebar: false,
            has_overview: false,
            has_rail: false,
            has_inspector: false,
            has_filter: false,
            has_controls: false,
            history: None,
            window_controls: None,
            header_in_bar: false,
            bar_revealed: false,
            bar_top: 0.0,
            titled: [false; 4],
            actions: [0; 4],
            leading: [false; 4],
            content_back: false,
            user_sidebar: None,
            sidebar: FlapState::open_when_docked(),
            inspector: FlapState::open_when_docked(),
            pane: NavPane::Content,
            push_pos: None,
            push_from: None,
            push_progress: 1.0,
            parts: Vec::new(),
            fit: Rc::new(RefCell::new(Default::default())),
            held: Held::default(),
        }
    }
}

impl Inputs {
    fn sidebar_width(&self) -> f64 {
        let m = &self.metrics;
        if self.mode == NavMode::Docked {
            return m.rail;
        }
        self.user_sidebar
            .map_or(m.sidebar, |authored| authored * m.scale)
            .clamp(m.sidebar_min, m.sidebar_max)
    }

    /// The narrowest the content still works at, for its kind.
    fn content_min(&self) -> f64 {
        let m = &self.metrics;
        match self.kind {
            ContentKind::List => m.list_min,
            ContentKind::Reading | ContentKind::Document => m.reading_min,
            ContentKind::Page => m.page_min,
            ContentKind::Grid => m.grid_min,
        }
    }

    /// The narrowest window the shell's compact layout works at: one
    /// column at a time (or the content under its drawer), never narrower
    /// than phone width; a rail stays beside the content.
    fn minimum_width(&self) -> f64 {
        let m = &self.metrics;
        let rail = if self.has_sidebar && self.mode == NavMode::Docked {
            m.rail
        } else {
            0.0
        };
        (rail + self.content_min()).max(m.compact_min)
    }

    fn header_wanted(&self, column: Column) -> bool {
        let i = column.index();
        self.titled.get(i).copied().unwrap_or(false)
            || self.actions.get(i).copied().unwrap_or(0) > 0
            || self.leading.get(i).copied().unwrap_or(false)
    }

    /// The header row lives in the window's bar now: it stands over the
    /// content, and only while the person reaches for the bar.
    fn header_in_window_bar(&self) -> bool {
        self.header_in_bar && self.window_controls.is_some()
    }
}

/// The shell's geometry for one size, in coordinates local to the shell.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NavGeometry {
    pub sidebar: SidebarPlacement,
    pub inspector: InspectorPlacement,
    pub overview: OverviewPlacement,
    /// The overview and content stand side by side.
    pub side_by_side: bool,
    /// The push stack's screens, first to last, when it has more than one.
    pub stack: Vec<NavPane>,
    pub header_height: f64,
    /// Width at or above which the sidebar docks.
    pub breakpoint: f64,
    /// Each column's visible area (header and body); a drawer's panel.
    pub sidebar_column: Rect,
    pub overview_column: Rect,
    pub content_column: Rect,
    pub inspector_column: Rect,
    /// The overview's rail, where the overview collapsed into it.
    pub rail: Rect,
    bodies: [Rect; 4],
    headers: [Rect; 4],
    pub filter: Rect,
    /// The band of the controls under the overview's header.
    pub controls: Rect,
    pub toggle: Rect,
    pub back: Rect,
    pub inspector_toggle: Rect,
    /// The window's History control, at the trailing edge of the row.
    pub history: Rect,
    /// Where the window's controls stand at the trailing end of the row,
    /// while the host merged the window's bar into it; else zero.
    pub window_controls: Rect,
    /// The window-wide row the window's bar covers (also while a header in
    /// the bar hides), or zero.
    pub window_row: Rect,
    /// The column whose header holds the sidebar toggle and the Back button.
    toggle_column: Option<Column>,
    back_column: Option<Column>,
    inspector_toggle_column: Option<Column>,
    history_column: Option<Column>,
    pub resize_handle: Rect,
}

impl NavGeometry {
    fn column(&self, column: Column) -> Rect {
        match column {
            Column::Sidebar => self.sidebar_column,
            Column::Overview => self.overview_column,
            Column::Content => self.content_column,
            Column::Inspector => self.inspector_column,
        }
    }

    fn body(&self, column: Column) -> Rect {
        self.bodies
            .get(column.index())
            .copied()
            .unwrap_or(Rect::ZERO)
    }

    fn header(&self, column: Column) -> Rect {
        self.headers
            .get(column.index())
            .copied()
            .unwrap_or(Rect::ZERO)
    }

    fn translated(&self, by: Vec2) -> Self {
        let t = |r: Rect| if r == Rect::ZERO { r } else { r + by };
        Self {
            sidebar_column: t(self.sidebar_column),
            overview_column: t(self.overview_column),
            content_column: t(self.content_column),
            inspector_column: t(self.inspector_column),
            rail: t(self.rail),
            bodies: self.bodies.map(t),
            headers: self.headers.map(t),
            filter: t(self.filter),
            controls: t(self.controls),
            toggle: t(self.toggle),
            back: t(self.back),
            inspector_toggle: t(self.inspector_toggle),
            history: t(self.history),
            window_controls: t(self.window_controls),
            window_row: t(self.window_row),
            resize_handle: t(self.resize_handle),
            ..self.clone()
        }
    }

    /// The parts this geometry shows, in build order (back to front). Behind
    /// an open drawer the other columns stay: laid out and painted, inert.
    fn visible_parts(&self, inputs: &Inputs) -> Vec<Part> {
        let mut parts = Vec::new();
        let shows = |r: Rect| r.width() > 0.0 && r.height() > 0.0;
        if inputs.has_rail && shows(self.rail) {
            parts.push(Part::Rail);
        }
        for column in [
            Column::Overview,
            Column::Content,
            Column::Inspector,
            Column::Sidebar,
        ] {
            let present = match column {
                Column::Sidebar => inputs.has_sidebar,
                Column::Overview => inputs.has_overview,
                Column::Content => true,
                Column::Inspector => inputs.has_inspector,
            };
            if !present || !shows(self.column(column)) {
                continue;
            }
            parts.push(Part::Body(column));
            let i = column.index();
            if shows(self.header(column)) {
                let docked_rail = inputs.mode == NavMode::Docked && column == Column::Sidebar;
                if inputs.titled.get(i).copied().unwrap_or(false) && !docked_rail {
                    parts.push(Part::Title(column));
                }
                if inputs.leading.get(i).copied().unwrap_or(false) {
                    parts.push(Part::Leading(column));
                }
                let actions = inputs.actions.get(i).copied().unwrap_or(0);
                for action in 0..actions {
                    parts.push(Part::Action(column, action));
                }
                if actions > 0 {
                    parts.push(Part::More(column));
                }
            }
            if inputs.has_filter && self.filter_column(inputs) == Some(column) && shows(self.filter)
            {
                parts.push(Part::Filter);
            }
            if inputs.has_controls && column == Column::Overview && shows(self.controls) {
                parts.push(Part::Controls);
            }
        }
        if shows(self.back) {
            parts.push(Part::Back);
        }
        if shows(self.inspector_toggle) {
            parts.push(Part::InspectorToggle);
        }
        if let Some(control) = inputs.history
            && shows(self.history)
        {
            parts.push(Part::History(control.is_open()));
        }
        if shows(self.toggle) {
            parts.push(Part::Toggle);
        }
        parts
    }

    /// Whether `p` is on the header row, which stays live beside a drawer.
    fn window_row_contains(&self, p: Point) -> bool {
        self.header_height > 0.0
            && Column::ALL
                .iter()
                .any(|column| self.header(*column).contains(p))
            || self.toggle.contains(p)
            || self.back.contains(p)
            || self.inspector_toggle.contains(p)
            || self.history.contains(p)
    }

    /// An open floating drawer, from either edge. A drawer is never modal:
    /// the content beside it stays visible, exposed and live, and a press
    /// on it puts the drawer away.
    fn open_drawer(&self) -> Option<Column> {
        [Column::Sidebar, Column::Inspector]
            .into_iter()
            .find(|column| self.floating(*column) && self.column(*column).width() > 0.0)
    }

    fn floating(&self, column: Column) -> bool {
        match column {
            Column::Sidebar => self.sidebar == SidebarPlacement::Drawer,
            Column::Inspector => self.inspector == InspectorPlacement::Drawer,
            Column::Overview | Column::Content => false,
        }
    }

    fn filter_column(&self, inputs: &Inputs) -> Option<Column> {
        if inputs.has_sidebar {
            Some(Column::Sidebar)
        } else if inputs.has_overview {
            Some(Column::Overview)
        } else {
            Some(Column::Content)
        }
    }

    /// Where the title (or, without one, the leading fill) of `column`'s
    /// header starts: after the shell's leading buttons in it, else at the
    /// content gutter (a sidebar's at the buttons' inset).
    fn lead_end(&self, column: Column, m: &Metrics) -> f64 {
        let header = self.header(column);
        let mut x = if column == Column::Sidebar {
            header.x0 + m.inset
        } else {
            header.x0 + m.row.gutter
        };
        for (button, home) in [
            (self.toggle, self.toggle_column),
            (self.back, self.back_column),
        ] {
            if home == Some(column) && button.width() > 0.0 {
                x = x.max(button.x1 + m.gap);
            }
        }
        x
    }

    /// The trailing end of `column`'s actions: before the shell's trailing
    /// buttons in it, one inset in.
    fn trail_start(&self, column: Column, m: &Metrics) -> f64 {
        self.actions_band(column).x1 - m.inset
    }

    /// The run of a header the app's actions may take: from the header's
    /// start to before the trailing shell buttons.
    fn actions_band(&self, column: Column) -> Rect {
        let header = self.header(column);
        if header == Rect::ZERO {
            return Rect::ZERO;
        }
        let mut x1 = header.x1;
        if self.window_controls.width() > 0.0 && header.x1 >= self.window_controls.x1 {
            x1 = x1.min(self.window_controls.x0);
        }
        for (button, home) in [
            (self.inspector_toggle, self.inspector_toggle_column),
            (self.history, self.history_column),
        ] {
            if home == Some(column) && button.width() > 0.0 {
                x1 = x1.min(button.x0);
            }
        }
        Rect::new(header.x0, header.y0, x1, header.y1)
    }
}

/// Compute the geometry for a shell of size `w × h`.
#[allow(
    clippy::too_many_lines,
    reason = "the responsive plan is one ordered decision: sidebar, inspector, split, stack, then rects"
)]
fn plan(inputs: &Inputs, w: f64, h: f64) -> NavGeometry {
    let m = inputs.metrics;
    let mut g = NavGeometry::default();
    if w <= 0.0 || h <= 0.0 {
        return g;
    }
    let ow = if inputs.has_overview { m.overview } else { 0.0 };
    let iw = if inputs.has_inspector {
        m.inspector
    } else {
        0.0
    };
    let held = inputs.held;
    let band = m.hysteresis;
    let content_min = inputs.content_min();
    let content_need = ow + content_min;
    let rail_sidebar = inputs.mode == NavMode::Docked;

    // 1. The sidebar docks while it fits beside the content's real minimum,
    //    shrinking from its width toward its own minimum first. Below that
    //    the window shows one column at a time (a push stack), or, for a
    //    content-primary app, floats the sidebar over the content.
    let preferred = inputs.sidebar_width().min(w);
    let narrowest = if rail_sidebar {
        preferred
    } else {
        m.sidebar_min.min(preferred)
    };
    g.breakpoint = if inputs.has_sidebar && !rail_sidebar {
        narrowest + content_need
    } else {
        0.0
    };
    g.sidebar = if !inputs.has_sidebar {
        SidebarPlacement::None
    } else if rail_sidebar || reaches(w, g.breakpoint, held.sidebar_docked, band) {
        SidebarPlacement::Docked
    } else if inputs.kind.is_content_primary() {
        SidebarPlacement::Drawer
    } else {
        SidebarPlacement::Screen
    };
    let sw = if g.sidebar == SidebarPlacement::Docked && !rail_sidebar {
        preferred.min(w - content_need).max(narrowest)
    } else {
        preferred
    };
    let sidebar_layout = layout_of(g.sidebar);
    let sidebar_open = inputs.sidebar.is_open(sidebar_layout);
    let sidebar_reveal = inputs.sidebar.reveal_in(sidebar_layout);
    let docked_target = if g.sidebar == SidebarPlacement::Docked && sidebar_open {
        sw
    } else {
        0.0
    };
    let docked_now = if g.sidebar == SidebarPlacement::Docked {
        sw * sidebar_reveal
    } else {
        0.0
    };

    // 2. The inspector docks while the content keeps its minimum beside it;
    //    narrower, it floats over the content from the trailing edge.
    g.inspector = if !inputs.has_inspector {
        InspectorPlacement::None
    } else if reaches(
        w - docked_target,
        content_need + iw,
        held.inspector_docked,
        band,
    ) {
        InspectorPlacement::Docked
    } else {
        InspectorPlacement::Drawer
    };
    let inspector_layout = inspector_layout_of(g.inspector);
    let inspector_open = inputs.inspector.is_open(inspector_layout);
    let inspector_reveal = inputs.inspector.reveal_in(inspector_layout);
    let inspector_target = if g.inspector == InspectorPlacement::Docked && inspector_open {
        iw
    } else {
        0.0
    };
    let inspector_now = if g.inspector == InspectorPlacement::Docked {
        iw * inspector_reveal
    } else {
        0.0
    };

    // 3. Overview and content side by side while both fit; where the list
    //    does not but its rail does, the overview collapses into the rail.
    //    Beside a docked sidebar the pair fits by construction.
    let region_target = w - docked_target - inspector_target;
    let beside_sidebar = g.sidebar == SidebarPlacement::Docked && !rail_sidebar;
    let list_fits = if beside_sidebar {
        region_target >= content_need - 1e-6
    } else {
        reaches(region_target, content_need, held.list_fits, band)
    };
    let rail_fits =
        inputs.has_rail && reaches(region_target, m.rail + content_min, held.rail_fits, band);
    g.overview = if !inputs.has_overview {
        OverviewPlacement::None
    } else if !list_fits && rail_fits {
        OverviewPlacement::Rail
    } else {
        OverviewPlacement::List
    };
    g.side_by_side = !inputs.has_overview || list_fits || rail_fits;

    // 4. The push stack.
    let mut stack = Vec::new();
    if g.sidebar == SidebarPlacement::Screen {
        stack.push(NavPane::Sidebar);
    }
    if inputs.has_overview && !g.side_by_side {
        stack.push(NavPane::Overview);
    }
    stack.push(NavPane::Content);
    let front = stack_index(&stack, inputs.pane);
    let pos = inputs.push_pos.map_or(f64_of_usize(front), |pos| {
        pos.clamp(0.0, f64_of_usize(stack.len().saturating_sub(1)))
    });
    g.stack = if stack.len() > 1 {
        stack.clone()
    } else {
        Vec::new()
    };

    // 5. The header row.
    let needs_header = inputs.has_sidebar
        || inputs.has_inspector
        || inputs.history.is_some()
        || Column::ALL.iter().any(|c| inputs.header_wanted(*c))
        || inputs.content_back
        || stack.len() > 1;
    // A header in the window's bar stands over the content and only while
    // the person reaches for the bar; the columns' bodies start at the top.
    let in_bar = inputs.header_in_window_bar();
    let row_h = if needs_header { m.header.min(h) } else { 0.0 };
    g.header_height = if in_bar && !inputs.bar_revealed {
        0.0
    } else {
        row_h
    };
    let hh = g.header_height;
    let bt = if in_bar { 0.0 } else { hh };
    // A header in the window's bar stands below whatever overlays the
    // window's top edge (the desktop's top bar while it shows).
    let top = if in_bar {
        inputs.bar_top.clamp(0.0, (h - row_h).max(0.0))
    } else {
        0.0
    };
    let header_of = |col: Rect| {
        if hh > 0.0 && col.width() > 0.0 {
            Rect::new(col.x0, col.y0 + top, col.x1, col.y0 + top + hh)
        } else {
            Rect::ZERO
        }
    };
    // A floating drawer stands under the header row, `s2` in.
    let drawer_top = if hh > 0.0 { top + hh + m.gap } else { m.gap };
    let drawer_bottom = (h - m.gap).max(drawer_top);

    // 6. Columns.
    let region = Rect::new(docked_now, 0.0, (w - inspector_now).max(docked_now), h);
    let region_w = region.width();
    let screen_x = |pane: NavPane| -> f64 {
        let i = stack_index(&stack, pane);
        (f64_of_usize(i) - pos) * w
    };
    // Sidebar.
    let mut sidebar_body = Rect::ZERO;
    let mut sidebar_header = Rect::ZERO;
    match g.sidebar {
        SidebarPlacement::None => {}
        SidebarPlacement::Docked => {
            if docked_now > 0.0 {
                g.sidebar_column = Rect::new(0.0, 0.0, docked_now, h);
                // The body keeps its width and slides under the edge, so its
                // rows never reflow while it collapses.
                sidebar_body = Rect::new(docked_now - sw, bt, docked_now, h);
                sidebar_header = header_of(g.sidebar_column);
            }
        }
        SidebarPlacement::Drawer => {
            if sidebar_reveal > 0.001 {
                // Keep a pointer-sized strip of the content exposed so the
                // drawer can be dismissed even when enlarged text fills it.
                let dw = m.drawer.min((w - m.gap - m.drawer_dismiss_band).max(0.0));
                let x0 = m.gap - (dw + m.gap) * (1.0 - sidebar_reveal);
                g.sidebar_column = Rect::new(x0, drawer_top, x0 + dw, drawer_bottom);
                (sidebar_header, sidebar_body) =
                    drawer_split(g.sidebar_column, hh, inputs.header_wanted(Column::Sidebar));
            }
        }
        SidebarPlacement::Screen => {
            let x = screen_x(NavPane::Sidebar);
            if x > -w && x < w {
                g.sidebar_column = Rect::new(x, 0.0, x + w, h);
                sidebar_body = Rect::new(x, bt, x + w, h);
                sidebar_header = header_of(g.sidebar_column);
            }
        }
    }
    // Overview and content.
    if g.side_by_side {
        let x = region.x0 + screen_x(NavPane::Content);
        let lead = match g.overview {
            OverviewPlacement::List => ow.min((region_w - content_min).max(0.0)),
            OverviewPlacement::Rail => m.rail.min(region_w),
            OverviewPlacement::None => 0.0,
        };
        match g.overview {
            OverviewPlacement::List => g.overview_column = Rect::new(x, 0.0, x + lead, h),
            OverviewPlacement::Rail => g.rail = Rect::new(x, 0.0, x + lead, h),
            OverviewPlacement::None => {}
        }
        g.content_column = Rect::new(x + lead, 0.0, x + region_w, h);
    } else {
        let ox = region.x0 + screen_x(NavPane::Overview);
        let cx = region.x0 + screen_x(NavPane::Content);
        if ox > -w && ox < w {
            g.overview_column = Rect::new(ox, 0.0, ox + region_w, h);
        }
        if cx > -w && cx < w {
            g.content_column = Rect::new(cx, 0.0, cx + region_w, h);
        }
    }
    // Inspector.
    let mut inspector_body = Rect::ZERO;
    let mut inspector_header = Rect::ZERO;
    match g.inspector {
        InspectorPlacement::None => {}
        InspectorPlacement::Docked => {
            if inspector_now > 0.0 {
                g.inspector_column = Rect::new(w - inspector_now, 0.0, w, h);
                inspector_body = Rect::new(w - inspector_now, bt, w - inspector_now + iw, h);
                inspector_header = header_of(g.inspector_column);
            }
        }
        InspectorPlacement::Drawer => {
            if inspector_reveal > 0.001 {
                let dw = iw.min((w - m.gap - m.drawer_dismiss_band).max(0.0));
                let x1 = w - m.gap + (dw + m.gap) * (1.0 - inspector_reveal);
                g.inspector_column = Rect::new(x1 - dw, drawer_top, x1, drawer_bottom);
                (inspector_header, inspector_body) = drawer_split(
                    g.inspector_column,
                    hh,
                    inputs.header_wanted(Column::Inspector),
                );
            }
        }
    }

    // 7. Headers and bodies.
    g.headers = [
        sidebar_header,
        header_of(g.overview_column),
        header_of(g.content_column),
        inspector_header,
    ];
    let body_of = |col: Rect| {
        if col.width() > 0.0 {
            Rect::new(col.x0, col.y0 + bt, col.x1, col.y1)
        } else {
            Rect::ZERO
        }
    };
    g.bodies = [
        sidebar_body,
        body_of(g.overview_column),
        body_of(g.content_column),
        inspector_body,
    ];
    if g.rail.width() > 0.0 {
        g.rail = Rect::new(g.rail.x0, bt, g.rail.x1, g.rail.y1);
    }

    // 8. The filter field and the overview's controls sit under their
    //    column's header, one control band each.
    let take_band = |g: &mut NavGeometry, column: Column| -> Rect {
        let body = g.body(column);
        if g.column(column).width() <= 0.0 || body.height() <= 0.0 {
            return Rect::ZERO;
        }
        let top = body.y0;
        let band = Rect::new(
            body.x0 + m.inset,
            top,
            (body.x1 - m.inset).max(body.x0 + m.inset),
            top + m.filter,
        );
        if let Some(slot) = g.bodies.get_mut(column.index()) {
            slot.y0 = (top + m.filter + m.gap).min(slot.y1);
        }
        band
    };
    if inputs.has_filter
        && let Some(column) = g.filter_column(inputs)
    {
        g.filter = take_band(&mut g, column);
    }
    if inputs.has_controls && inputs.has_overview {
        g.controls = take_band(&mut g, Column::Overview);
    }

    // 9. The shell's own buttons.

    let front_pane = stack.get(front).copied().unwrap_or(NavPane::Content);
    let front_column = match front_pane {
        NavPane::Sidebar => Column::Sidebar,
        NavPane::Overview => Column::Overview,
        NavPane::Content if inputs.has_overview && g.overview_column.width() > 0.0 => {
            Column::Overview
        }
        NavPane::Content => Column::Content,
    };
    let push_back = stack.len() > 1 && front > 0;
    let window_row = Rect::new(0.0, top, w, top + hh);
    if hh > 0.0
        && !push_back
        && inputs.has_sidebar
        && matches!(
            g.sidebar,
            SidebarPlacement::Docked | SidebarPlacement::Drawer
        )
    {
        // Pinned to the window's top-leading corner, on the sidebar while it
        // shows docked and on the next column otherwise, so it never chases
        // a sliding edge (a drawer opens under the row, beside it).
        g.toggle = m.row.leading_chip(window_row, 0);
        g.toggle_column = Some(
            if g.sidebar == SidebarPlacement::Docked && g.sidebar_column.x1 >= g.toggle.x1 {
                Column::Sidebar
            } else {
                front_column
            },
        );
    }
    let back_column = if push_back {
        Some(front_column)
    } else if inputs.content_back && g.content_column.width() > 0.0 {
        Some(Column::Content)
    } else {
        None
    };
    if let Some(column) = back_column
        && (push_back || g.header(column) != Rect::ZERO)
    {
        let slot = usize::from(g.toggle_column == Some(column) && g.toggle.width() > 0.0);
        // The page moves; navigation belongs to the window. Back must stay
        // operable even before the incoming screen enters the viewport.
        let row = if push_back {
            window_row
        } else {
            g.header(column)
        };
        g.back = if slot == 0 {
            m.row.leading_chip(row, 0)
        } else {
            // After the toggle, which is pinned to the window's corner.
            let x = g.toggle.x1 + m.gap;
            Rect::new(x, g.toggle.y0, x + m.chip, g.toggle.y1)
        };
        g.back_column = Some(column);
    }
    // The window's controls take the row's trailing end while the host
    // merged the window's bar into it: one bar, the app's header and the
    // window's controls side by side. The shell's own trailing buttons and
    // the actions stand before them.
    if let Some(width) = inputs.window_controls
        && row_h > 0.0
    {
        g.window_row = Rect::new(0.0, top, w, top + row_h);
        let x1 = (w - m.inset).max(0.0);
        g.window_controls = Rect::new((x1 - width).max(0.0), top, x1, top + row_h);
    }
    // Where a column's trailing shell buttons count from: its header's end,
    // or before the window's controls in the header that reaches them.
    let trailing_row = |g: &NavGeometry, header: Rect| -> Rect {
        if g.window_controls.width() > 0.0 && header.x1 >= g.window_controls.x1 {
            Rect::new(
                header.x0,
                header.y0,
                (g.window_controls.x0 - m.gap + m.inset).max(header.x0),
                header.y1,
            )
        } else {
            header
        }
    };
    // History stands in the window's top-trailing corner: the trailing edge
    // of the column that reaches the window's trailing edge (never a drawer).
    if inputs.history.is_some()
        && let Some(column) = [
            Column::Inspector,
            Column::Content,
            Column::Overview,
            Column::Sidebar,
        ]
        .into_iter()
        .find(|column| {
            let header = g.header(*column);
            !g.floating(*column)
                && g.column(*column).width() > 0.0
                && header.x0 < w - 0.5
                && header.x1 >= w - 0.5
        })
    {
        g.history_column = Some(column);
        g.history = m.row.trailing_chip(trailing_row(&g, g.header(column)), 0);
    }
    if inputs.has_inspector && hh > 0.0 {
        // Docked and open, the toggle heads the inspector; otherwise it
        // stands in the content's header (a drawer opens under it).
        let column = if g.inspector == InspectorPlacement::Docked && inspector_open {
            Column::Inspector
        } else if g.content_column.width() > 0.0 {
            Column::Content
        } else {
            front_column
        };
        let row = g.header(column);
        if row != Rect::ZERO && g.column(column).width() > 0.0 {
            g.inspector_toggle_column = Some(column);
            let slot = usize::from(g.history_column == Some(column));
            g.inspector_toggle = m.row.trailing_chip(trailing_row(&g, row), slot);
        }
    }
    // Content is fully hidden by a pushed screen: no buttons on it.
    if g.toggle_column.is_some_and(|c| g.column(c).width() <= 0.0) {
        g.toggle = Rect::ZERO;
        g.toggle_column = None;
    }

    // 10. Dragging the docked sidebar's trailing edge resizes it.
    if g.sidebar == SidebarPlacement::Docked
        && inputs.mode != NavMode::Docked
        && sidebar_open
        && g.sidebar_column.width() > 0.0
    {
        let x = g.sidebar_column.x1;
        g.resize_handle = Rect::new(x - m.resize_grip, bt, x + m.resize_grip, h);
    }
    g
}

/// A floating drawer's header (when its column has one) and body.
fn drawer_split(col: Rect, hh: f64, header: bool) -> (Rect, Rect) {
    if col.width() <= 0.0 {
        return (Rect::ZERO, Rect::ZERO);
    }
    if header && hh > 0.0 {
        let y1 = (col.y0 + hh).min(col.y1);
        (
            Rect::new(col.x0, col.y0, col.x1, y1),
            Rect::new(col.x0, y1, col.x1, col.y1),
        )
    } else {
        (Rect::ZERO, col)
    }
}

fn layout_of(sidebar: SidebarPlacement) -> SplitLayout {
    match sidebar {
        SidebarPlacement::Drawer | SidebarPlacement::Screen => SplitLayout::Overlay,
        SidebarPlacement::Docked | SidebarPlacement::None => SplitLayout::Docked,
    }
}

fn inspector_layout_of(inspector: InspectorPlacement) -> SplitLayout {
    match inspector {
        InspectorPlacement::Drawer => SplitLayout::Overlay,
        InspectorPlacement::Docked | InspectorPlacement::None => SplitLayout::Docked,
    }
}

fn stack_index(stack: &[NavPane], pane: NavPane) -> usize {
    stack
        .iter()
        .position(|p| *p == pane)
        .or_else(|| {
            // An overview standing beside the content is the content screen.
            (pane == NavPane::Overview)
                .then(|| stack.iter().position(|p| *p == NavPane::Content))
                .flatten()
        })
        .unwrap_or(stack.len().saturating_sub(1))
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "a reveal is clamped to 0..=1, always well within f32 range"
)]
fn unit_f32(v: f64) -> f32 {
    v.clamp(0.0, 1.0) as f32
}

/// The shell's buttons report presses here.
#[derive(Default)]
struct Requests {
    toggle: Cell<bool>,
    back: Cell<bool>,
    inspector: Cell<bool>,
}

/// A drag of the sidebar's trailing edge.
#[derive(Clone, Copy, Debug)]
struct Resize {
    start_x: f64,
    start_width: f64,
}

/// An app's own page stack, which the shell's Back chip pops.
struct ContentBack {
    can_go_back: Box<dyn Fn() -> bool>,
    go_back: Box<dyn FnMut()>,
}

/// An open/closed state the app keeps in a [`LocalState`].
struct OpenBinding {
    binding: LocalBinding<bool>,
    /// A value to apply once the layout knows the placement.
    wanted: Option<bool>,
}

/// What the host walks in place of a push screen that is not in front:
/// no focus, no semantics, no children. The part itself is still laid out,
/// painted, synced and updated by the shell.
#[derive(Debug, Default)]
struct InertPart;

static INERT_PART: InertPart = InertPart;

impl Widget for InertPart {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(Style::default(), node_map, self)
    }

    fn name(&self) -> &'static str {
        "NavShellInert"
    }

    fn is_layout_dirty(&self) -> bool {
        false
    }
}

/// The one window shell. See the module docs.
#[allow(
    clippy::struct_excessive_bools,
    reason = "structure, layout and paint dirtiness and the cross-fade are independent flags"
)]
pub struct NavShell {
    id: Option<String>,
    sidebar: Option<Element>,
    overview: Option<Element>,
    content: Element,
    inspector: Option<Element>,
    rail: Option<Element>,
    filter: Option<SearchField>,
    controls: Option<Element>,
    headers: [Header; 4],
    pending: Pending,
    toggle: Button,
    back: Button,
    inspector_toggle: Button,
    /// The window's History control: the one that opens History and the
    /// one, on, that closes it.
    history_open: Button,
    history_close: Button,
    requests: Rc<Requests>,
    pane_binding: Option<LocalBinding<NavPane>>,
    sidebar_state: Option<OpenBinding>,
    inspector_state: Option<OpenBinding>,
    content_back: Option<ContentBack>,
    /// The content pane the person last worked in; a collapse to narrow
    /// keeps it in front.
    active_hint: Option<NavPane>,

    inputs: Rc<RefCell<Inputs>>,
    policy: Rc<crate::widget::manual::Policy>,
    invalidation: ManualInvalidation,
    baseline: Vec<(NodeId, Style)>,
    /// The parts in the host tree, in build order.
    host_parts: Vec<Part>,
    /// What the host meets in place of a part of a screen not in front, one
    /// per part in the tree.
    inert_parts: Vec<InertPart>,
    layout_theme: Theme,
    conventions: PlatformConventions,

    rect: Rect,
    geometry: NavGeometry,
    last_stack: Option<Vec<NavPane>>,
    /// The app or the person chose the front screen.
    pane_chosen: bool,
    resize: Option<Resize>,
    last_cursor: Point,
    /// When the push or the reduced-motion fade in flight last advanced, or
    /// began; `None` at rest. The first frame covers the time since the
    /// change was asked for, never an idle gap before it.
    last_tick: Option<Instant>,
    /// Reduced motion: the sidebar and drawers fade in instead of sliding.
    fade: f64,
    /// How far the columns a layout switch brought in have faded in, `0..=1`
    /// (`1` at rest): the columns that appeared or moved where no slide
    /// carries them, and under reduced motion every one of them.
    switch_fade: f64,
    /// Which columns ([`Column::index`]) the layout switch fades in.
    switch_columns: [bool; 4],
    animating: bool,
    /// The host tree has been built once.
    built: bool,
    structure_dirty: bool,
    layout_dirty: bool,
    paint_dirty: bool,
    /// The window's transient notices, over its bottom trailing corner.
    toasts: ToastStack,
    /// Every pixel the notices painted in the last draw, their shadows
    /// included, and what they painted there.
    toasts_painted: Option<Rect>,
    toasts_scene: Scene,
    /// [`Self::toasts_painted`] and [`Self::toasts_scene`] as of the last
    /// frame the host presented ([`Widget::mark_drawn`]).
    toasts_drawn: Option<Rect>,
    toasts_drawn_scene: Scene,
    /// A notice came, went or changed: the next draw paints it.
    toasts_dirty: bool,
    /// The window's sync status, quiet text before the trailing controls
    /// of the row History stands in.
    sync_status: crate::widget::HeaderStatus,
    /// The room that row leaves before the sync status, where a document
    /// says how its save is going ([`Self::header_status_slot`]).
    status_slot: Rect,
}

impl Default for NavShell {
    fn default() -> Self {
        Self::new()
    }
}

impl NavShell {
    /// An empty shell; give it columns with the builders.
    pub fn new() -> Self {
        let requests = Rc::new(Requests::default());
        let inputs = Rc::new(RefCell::new(Inputs::default()));
        let policy_inputs = Rc::clone(&inputs);
        let policy: Rc<crate::widget::manual::Policy> =
            Rc::new(move |ctx: &mut ManualContext<'_>| place_parts(&policy_inputs.borrow(), ctx));
        let toggle = Self::shell_button("nav-shell-sidebar", Icon::PanelLeft, "Hide sidebar", {
            let requests = Rc::clone(&requests);
            move || requests.toggle.set(true)
        });
        let back = Self::shell_button("nav-shell-back", Icon::ChevronLeft, "Back", {
            let requests = Rc::clone(&requests);
            move || requests.back.set(true)
        });
        let inspector_toggle =
            Self::shell_button("nav-shell-inspector", Icon::PanelRight, "Hide inspector", {
                let requests = Rc::clone(&requests);
                move || requests.inspector.set(true)
            });
        Self {
            id: None,
            sidebar: None,
            overview: None,
            content: Box::new(crate::widget::Spacer::new()),
            inspector: None,
            rail: None,
            filter: None,
            controls: None,
            headers: Column::ALL.map(Header::new),
            pending: Pending::default(),
            toggle,
            back,
            inspector_toggle,
            history_open: crate::standalone_history_host::history_control_button(
                "nav-shell-history",
                HistoryControl::Closed,
            ),
            history_close: crate::standalone_history_host::history_control_button(
                "nav-shell-history",
                HistoryControl::Open,
            ),
            requests,
            pane_binding: None,
            sidebar_state: None,
            inspector_state: None,
            content_back: None,
            active_hint: None,
            inputs,
            policy,
            invalidation: ManualInvalidation::default(),
            baseline: Vec::new(),
            host_parts: Vec::new(),
            inert_parts: Vec::new(),
            layout_theme: Theme::dark(),
            conventions: PlatformConventions::default(),
            rect: Rect::ZERO,
            geometry: NavGeometry::default(),
            last_stack: None,
            pane_chosen: false,
            resize: None,
            last_cursor: Point::ZERO,
            last_tick: None,
            fade: 1.0,
            switch_fade: 1.0,
            switch_columns: [false; 4],
            animating: false,
            built: false,
            structure_dirty: false,
            layout_dirty: true,
            paint_dirty: true,
            toasts: ToastStack::new().with_max(1),
            toasts_painted: None,
            toasts_scene: Scene::new(),
            toasts_drawn: None,
            toasts_drawn_scene: Scene::new(),
            toasts_dirty: false,
            sync_status: crate::widget::HeaderStatus::default(),
            status_slot: Rect::ZERO,
        }
    }

    fn shell_button(id: &str, icon: Icon, name: &str, on_press: impl FnMut() + 'static) -> Button {
        Button::icon_only(id, icon, name).on_view(on_press)
    }

    // --- notices -----------------------------------------------------------

    /// Show a transient notice over the window's bottom trailing corner: an
    /// operation's result or failure. It replaces the one showing, expires on
    /// its own (held while the pointer rests on it and under the capture
    /// clock), and a click dismisses it.
    pub fn toast(&mut self, text: impl Into<String>, kind: ToastKind) {
        self.toasts.push(text, kind);
        self.toasts_dirty = true;
    }

    /// Show a notice with one button, `action` ("Undo"), beside its text.
    /// [`Self::take_toast_action`] reports the press.
    pub fn toast_with_action(
        &mut self,
        text: impl Into<String>,
        kind: ToastKind,
        action: impl Into<String>,
    ) {
        self.toasts.push_with_action(text, kind, action);
        self.toasts_dirty = true;
    }

    /// The text of the notice whose button the person pressed since the
    /// last call.
    pub fn take_toast_action(&mut self) -> Option<String> {
        self.toasts.take_activated()
    }

    /// Take down the notice showing, when its news no longer holds.
    pub fn dismiss_toast(&mut self) {
        if !self.toasts.is_empty() {
            self.toasts.dismiss(0);
            self.toasts_dirty = true;
        }
    }

    /// The notice showing, if any.
    pub fn toast_text(&self) -> Option<&str> {
        self.toasts.iter().next().map(|toast| toast.text.as_str())
    }

    /// Whether the notices painted anything other than what the host last
    /// presented: one came, went, expired, moved or changed its look.
    fn toasts_changed(&self) -> bool {
        self.toasts_painted != self.toasts_drawn
            || !self.toasts_scene.encoding_eq(&self.toasts_drawn_scene)
    }

    fn toasts_expired(&self) -> bool {
        self.toasts
            .next_wake_deadline()
            .is_some_and(|deadline| deadline <= crate::time::now())
    }

    // --- slots -------------------------------------------------------------

    /// The sidebar of destinations (usually a [`crate::widget::NavList`]).
    #[must_use]
    pub fn sidebar(mut self, sidebar: Element) -> Self {
        self.set_sidebar(Some(sidebar));
        self
    }

    /// The overview list beside the content (a message list, a city list):
    /// content the person reads, not navigation.
    #[must_use]
    pub fn overview(mut self, overview: Element) -> Self {
        self.set_overview(Some(overview));
        self
    }

    /// What the overview collapses into where the list no longer fits
    /// beside the content but a rail does: a [`crate::widget::NavList::rail`]
    /// of the same places, so the window narrows to rail and detail before
    /// it pushes.
    #[must_use]
    pub fn overview_rail(mut self, rail: Element) -> Self {
        self.set_overview_rail(Some(rail));
        self
    }

    /// The content.
    #[must_use]
    pub fn content(mut self, content: Element) -> Self {
        self.set_content(content);
        self
    }

    /// The inspector at the trailing edge: properties of the selection.
    #[must_use]
    pub fn inspector(mut self, inspector: Element) -> Self {
        self.set_inspector(Some(inspector));
        self
    }

    /// Controls that act on the overview list (its sort order, a scope),
    /// under the overview's header at the shell's inset.
    #[must_use]
    pub fn overview_controls(mut self, controls: Element) -> Self {
        self.set_overview_controls(Some(controls));
        self
    }

    /// The sidebar column's header: a sentence-case title and the app's
    /// actions (buttons). The toggle is the shell's; do not add one.
    #[must_use]
    pub fn sidebar_header(mut self, title: impl Into<String>, actions: Vec<Element>) -> Self {
        self.set_sidebar_header(title, actions);
        self
    }

    /// The overview column's header (the list's title and its actions).
    #[must_use]
    pub fn overview_header(mut self, title: impl Into<String>, actions: Vec<Element>) -> Self {
        self.set_overview_header(title, actions);
        self
    }

    /// The content column's header: the detail title and toolbar actions.
    #[must_use]
    pub fn content_header(mut self, title: impl Into<String>, actions: Vec<Element>) -> Self {
        self.set_content_header(title, actions);
        self
    }

    /// The content's header with its title bound to `title`: a view names
    /// its content from resource state, and the header follows the state.
    #[must_use]
    pub fn content_header_bound(
        mut self,
        title: &LocalState<String>,
        actions: Vec<Element>,
    ) -> Self {
        let (value, binding) = LocalBinding::new(title);
        self.set_content_header(value, actions);
        if let Some(header) = self.headers.get_mut(Column::Content.index()) {
            header.binding = Some(binding);
        }
        self
    }

    /// The fill of the content's header between the shell's leading buttons
    /// and the actions: a path bar or breadcrumb, or a mode or tab switcher
    /// with no title. It stands at its own width, and gets the room there is
    /// when it needs more.
    #[must_use]
    pub fn content_header_leading(mut self, leading: Element) -> Self {
        self.set_content_header_leading(Some(leading));
        self
    }

    /// The inspector column's header.
    #[must_use]
    pub fn inspector_header(mut self, title: impl Into<String>, actions: Vec<Element>) -> Self {
        self.set_inspector_header(title, actions);
        self
    }

    /// How many items the overview lists, after its title.
    #[must_use]
    pub fn overview_count(mut self, count: usize) -> Self {
        self.set_count(Column::Overview, Some(count));
        self
    }

    /// How many items the content shows, after its title.
    #[must_use]
    pub fn content_count(mut self, count: usize) -> Self {
        self.set_count(Column::Content, Some(count));
        self
    }

    /// The one filter field of the window, scoped to the sidebar's list (or
    /// the overview when there is no sidebar, else the content), at the
    /// shell's metrics. Ctrl+F (⌘F) focuses it. The shell carries its query
    /// to the column's [`crate::widget::Filterable`] after every event and
    /// keeps the field: read it back with [`Self::filter`].
    #[must_use]
    pub fn with_filter(mut self, filter: SearchField) -> Self {
        self.filter = Some(filter);
        self.slots_changed();
        self
    }

    /// What the sidebar becomes in a narrow window: the first push screen
    /// (the default), or an icon rail docked at every width.
    #[must_use]
    pub fn mode(mut self, mode: NavMode) -> Self {
        self.inputs.borrow_mut().mode = mode;
        self.slots_changed();
        self
    }

    /// What the content is ([`ContentKind`]): the shell docks the sidebar
    /// (and the inspector, and the overview beside the content) while the
    /// content keeps the minimum its kind needs, and the window's narrowest
    /// width is that minimum's compact layout.
    #[must_use]
    pub fn content_kind(mut self, kind: ContentKind) -> Self {
        self.inputs.borrow_mut().kind = kind;
        self.slots_changed();
        self
    }

    /// The narrowest window this shell works at, in logical pixels: its
    /// compact layout, one column at a time, at the theme it was last
    /// built with ([`Widget::minimum_window_width`]).
    pub fn minimum_width(&self) -> f64 {
        self.inputs.borrow().minimum_width()
    }

    /// Give the shell a stable id for tests and inspection; its headings
    /// group is keyed by it on the agent surface.
    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Put the header row in the window's bar: while the window is
    /// maximized, the row hides with the window's controls and appears
    /// with them over the top of the content when the person reaches for
    /// the bar (`widget::top_edge_reveal`), as one bar. In a tiled window
    /// the row stands above the content as usual.
    #[must_use]
    pub fn with_header_in_window_bar(self) -> Self {
        self.inputs.borrow_mut().header_in_bar = true;
        self
    }

    /// Start with the sidebar hidden (where it docks).
    #[must_use]
    pub fn with_sidebar_hidden(self) -> Self {
        self.inputs.borrow_mut().sidebar.snap(false);
        self
    }

    /// Start with the inspector hidden (where it docks).
    #[must_use]
    pub fn with_inspector_hidden(self) -> Self {
        self.inputs.borrow_mut().inspector.snap(false);
        self
    }

    /// Keep whether the sidebar shows in `state`: the shell writes where it
    /// stands once laid out and every choice the person makes; setting the
    /// state shows or hides the sidebar (opens the drawer in a narrow
    /// window, from `ViewCx::on_stage` for a capture).
    #[must_use]
    pub fn with_sidebar_state(mut self, state: &LocalState<bool>) -> Self {
        let (_, binding) = LocalBinding::new(state);
        self.sidebar_state = Some(OpenBinding {
            binding,
            wanted: None,
        });
        self
    }

    /// Keep whether the inspector shows in `state`, as
    /// [`Self::with_sidebar_state`].
    #[must_use]
    pub fn with_inspector_state(mut self, state: &LocalState<bool>) -> Self {
        let (_, binding) = LocalBinding::new(state);
        self.inspector_state = Some(OpenBinding {
            binding,
            wanted: None,
        });
        self
    }

    /// Keep the front screen of a narrow window in `state`: a row of the
    /// overview can push the content by setting [`NavPane::Content`], and
    /// the Back button writes the screen it returns to.
    #[must_use]
    pub fn with_pane_state(mut self, state: &LocalState<NavPane>) -> Self {
        let (pane, binding) = LocalBinding::new(state);
        self.inputs.borrow_mut().pane = pane;
        self.pane_binding = Some(binding);
        self.pane_chosen = true;
        self
    }

    /// Bind the content's Back to the app's page stack: while `stack` can go
    /// back the shell shows its Back chip at the leading edge of the
    /// content's header, and pressing it pops the stack (browse history, a
    /// drill-down from a dashboard into a queue). A narrow window's push
    /// stack shares the chip: Back pops the app's pages first.
    #[must_use]
    pub fn with_back<T>(self, stack: &LocalState<crate::NavStack<T>>) -> Self
    where
        T: Clone + PartialEq + 'static,
    {
        let read = stack.clone();
        let write = stack.clone();
        self.on_back(
            move || read.with(crate::NavStack::can_go_back),
            move || {
                write.update(|stack| {
                    stack.back();
                });
            },
        )
    }

    /// The content's Back for a page stack the app keeps itself:
    /// `can_go_back` is asked every frame, `go_back` runs on a press.
    #[must_use]
    pub fn on_back(
        mut self,
        can_go_back: impl Fn() -> bool + 'static,
        go_back: impl FnMut() + 'static,
    ) -> Self {
        let can = can_go_back();
        self.content_back = Some(ContentBack {
            can_go_back: Box::new(can_go_back),
            go_back: Box::new(go_back),
        });
        self.inputs.borrow_mut().content_back = can;
        self.slots_changed();
        self
    }

    // --- live slot swap ----------------------------------------------------
    //
    // A host that rebuilds its children keeps the shell and swaps them in
    // here: the shell's responsive state lives on the shell. A swap made
    // while the host holds the tree waits for the next build.

    pub fn set_sidebar(&mut self, sidebar: Option<Element>) {
        self.stage_body(Column::Sidebar, sidebar);
    }

    pub fn set_overview(&mut self, overview: Option<Element>) {
        self.stage_body(Column::Overview, overview);
    }

    pub fn set_content(&mut self, content: Element) {
        self.stage_body(Column::Content, Some(content));
    }

    pub fn set_inspector(&mut self, inspector: Option<Element>) {
        self.stage_body(Column::Inspector, inspector);
    }

    pub fn set_overview_rail(&mut self, rail: Option<Element>) {
        self.pending.rail = Some(Staged(rail));
        self.pending_changed();
    }

    pub fn set_overview_controls(&mut self, controls: Option<Element>) {
        self.pending.controls = Some(Staged(controls));
        self.pending_changed();
    }

    pub fn set_content_header_leading(&mut self, leading: Option<Element>) {
        if let Some(slot) = self.pending.leading.get_mut(Column::Content.index()) {
            *slot = Some(Staged(leading));
        }
        self.pending_changed();
    }

    fn stage_body(&mut self, column: Column, element: Option<Element>) {
        if let Some(slot) = self.pending.bodies.get_mut(column.index()) {
            *slot = Some(Staged(element));
        }
        self.pending_changed();
    }

    /// Change the sidebar's header in place: its title and actions.
    pub fn set_sidebar_header(&mut self, title: impl Into<String>, actions: Vec<Element>) {
        self.stage_header(Column::Sidebar, title.into(), actions);
    }

    /// Change the overview's header in place.
    pub fn set_overview_header(&mut self, title: impl Into<String>, actions: Vec<Element>) {
        self.stage_header(Column::Overview, title.into(), actions);
    }

    /// Change the content's header in place (the selected message's subject
    /// and what can be done to it).
    pub fn set_content_header(&mut self, title: impl Into<String>, actions: Vec<Element>) {
        self.stage_header(Column::Content, title.into(), actions);
    }

    /// Change the inspector's header in place.
    pub fn set_inspector_header(&mut self, title: impl Into<String>, actions: Vec<Element>) {
        self.stage_header(Column::Inspector, title.into(), actions);
    }

    fn stage_header(&mut self, column: Column, title: String, actions: Vec<Element>) {
        self.set_title(column, title);
        if let Some(slot) = self.pending.actions.get_mut(column.index()) {
            *slot = Some(actions);
        }
        self.pending_changed();
    }

    /// Change the sidebar's title in place.
    pub fn set_sidebar_title(&mut self, title: impl Into<String>) {
        self.set_title(Column::Sidebar, title.into());
    }

    /// Change the overview's title in place.
    pub fn set_overview_title(&mut self, title: impl Into<String>) {
        self.set_title(Column::Overview, title.into());
    }

    /// Change the content's title in place (the selected message's subject).
    pub fn set_content_title(&mut self, title: impl Into<String>) {
        self.set_title(Column::Content, title.into());
    }

    /// Change the inspector's title in place.
    pub fn set_inspector_title(&mut self, title: impl Into<String>) {
        self.set_title(Column::Inspector, title.into());
    }

    /// Show or clear the count after the content's title.
    pub fn set_content_count(&mut self, count: Option<usize>) {
        self.set_count(Column::Content, count);
    }

    /// Whether the content's page stack can go back, for an app that keeps
    /// it itself ([`Self::on_back`]).
    pub fn set_can_go_back(&mut self, can: bool) {
        let changed = {
            let mut inputs = self.inputs.borrow_mut();
            let changed = inputs.content_back != can;
            inputs.content_back = can;
            changed
        };
        if changed {
            self.structure_dirty = true;
            self.invalidate();
        }
    }

    fn set_title(&mut self, column: Column, title: String) {
        let title = (!title.trim().is_empty()).then_some(title);
        if let Some(header) = self.headers.get_mut(column.index())
            && header.title.title != title
        {
            header.title.title = title;
            self.slots_changed();
        }
    }

    fn set_count(&mut self, column: Column, count: Option<usize>) {
        if let Some(header) = self.headers.get_mut(column.index())
            && header.title.count != count
        {
            header.title.count = count;
            self.invalidate();
            self.structure_dirty = true;
        }
    }

    /// A slot or header waits for the next build; before the first one there
    /// is no tree to wait for.
    fn pending_changed(&mut self) {
        if !self.built {
            self.apply_pending();
        }
        self.slots_changed();
    }

    /// Put the waiting slots and headers in place, dropping what they
    /// replace. Only before the first build or from `build_layout`, when the
    /// host holds no part of the old tree.
    fn apply_pending(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let pending = std::mem::take(&mut self.pending);
        let Pending {
            bodies,
            actions,
            leading,
            controls,
            rail,
        } = pending;
        for (column, body) in Column::ALL.into_iter().zip(bodies) {
            let Some(Staged(body)) = body else { continue };
            match column {
                Column::Sidebar => self.sidebar = body,
                Column::Overview => self.overview = body,
                Column::Content => {
                    if let Some(content) = body {
                        self.content = content;
                    }
                }
                Column::Inspector => self.inspector = body,
            }
        }
        for (header, actions) in self.headers.iter_mut().zip(actions) {
            if let Some(actions) = actions {
                header.more.close();
                header.actions = actions;
            }
        }
        for (header, leading) in self.headers.iter_mut().zip(leading) {
            if let Some(Staged(leading)) = leading {
                header.leading = leading;
            }
        }
        if let Some(Staged(controls)) = controls {
            self.controls = controls;
        }
        if let Some(Staged(rail)) = rail {
            self.rail = rail;
        }
        self.reset_fit();
    }

    /// Forget how the actions fit: new actions are measured afresh.
    fn reset_fit(&mut self) {
        let inputs = self.inputs.borrow();
        let mut fit = inputs.fit.borrow_mut();
        for (slot, header) in fit.iter_mut().zip(&mut self.headers) {
            let capable: Vec<bool> = header
                .actions
                .iter_mut()
                .map(|action| {
                    action
                        .as_any()
                        .and_then(|any| any.downcast_ref::<Button>())
                        .is_some_and(Button::compacts_to_icon)
                })
                .collect();
            let compact_now: Vec<bool> = header
                .actions
                .iter()
                .map(|action| {
                    action
                        .as_any()
                        .and_then(|any| any.downcast_ref::<Button>())
                        .is_some_and(Button::is_compact_presentation)
                })
                .collect();
            if slot.capable != capable || slot.compact_now.len() != compact_now.len() {
                *slot = ColumnFit {
                    full: vec![None; capable.len()],
                    capable,
                    compact_now,
                    compact: false,
                    shown: None,
                };
            } else {
                slot.compact_now = compact_now;
            }
        }
    }

    fn slots_changed(&mut self) {
        {
            let mut inputs = self.inputs.borrow_mut();
            let has = |current: bool, pending: Option<&Staged>| match pending {
                Some(next) => next.is_some(),
                None => current,
            };
            inputs.has_sidebar = has(
                self.sidebar.is_some(),
                at(&self.pending.bodies, Column::Sidebar).as_ref(),
            );
            inputs.has_overview = has(
                self.overview.is_some(),
                at(&self.pending.bodies, Column::Overview).as_ref(),
            );
            inputs.has_inspector = has(
                self.inspector.is_some(),
                at(&self.pending.bodies, Column::Inspector).as_ref(),
            );
            inputs.has_rail = has(self.rail.is_some(), self.pending.rail.as_ref());
            inputs.has_controls = has(self.controls.is_some(), self.pending.controls.as_ref());
            inputs.has_filter = self.filter.is_some();
            for column in Column::ALL {
                let header = at(&self.headers, column);
                *at_mut(&mut inputs.titled, column) = header.title.is_shown();
                *at_mut(&mut inputs.actions, column) = at(&self.pending.actions, column)
                    .as_ref()
                    .map_or(header.actions.len(), Vec::len);
                *at_mut(&mut inputs.leading, column) = has(
                    header.leading.is_some(),
                    at(&self.pending.leading, column).as_ref(),
                );
            }
        }
        self.structure_dirty = true;
        self.invalidate();
    }

    fn invalidate(&mut self) {
        self.invalidation.invalidate();
        self.layout_dirty = true;
        self.paint_dirty = true;
    }

    // --- typed access ------------------------------------------------------

    /// The sidebar as the widget the app gave it (the one waiting to be
    /// shown, if a swap is pending), through its `Widget::as_any_mut`
    /// (which the widget answers with `Some(self)`).
    pub fn sidebar_mut<T: Widget + 'static>(&mut self) -> Option<&mut T> {
        let slot = match at_mut(&mut self.pending.bodies, Column::Sidebar) {
            Some(Staged(pending)) => pending.as_deref_mut(),
            None => self.sidebar.as_deref_mut(),
        };
        downcast(slot)
    }

    /// The sidebar as the widget the app gave it, to read: the one waiting
    /// to be shown, if a swap is pending.
    pub fn sidebar_ref<T: Widget + 'static>(&self) -> Option<&T> {
        let [sidebar, ..] = &self.pending.bodies;
        let slot = match sidebar {
            Some(Staged(pending)) => pending.as_deref(),
            None => self.sidebar.as_deref(),
        };
        slot.and_then(|widget| widget.as_any())
            .and_then(|any| any.downcast_ref::<T>())
    }

    /// The overview as the widget the app gave it.
    pub fn overview_mut<T: Widget + 'static>(&mut self) -> Option<&mut T> {
        let slot = match at_mut(&mut self.pending.bodies, Column::Overview) {
            Some(Staged(pending)) => pending.as_deref_mut(),
            None => self.overview.as_deref_mut(),
        };
        downcast(slot)
    }

    /// The content as the widget the app gave it.
    pub fn content_mut<T: Widget + 'static>(&mut self) -> Option<&mut T> {
        let slot = match at_mut(&mut self.pending.bodies, Column::Content) {
            Some(Staged(pending)) => pending.as_deref_mut(),
            None => Some(self.content.as_mut()),
        };
        downcast(slot)
    }

    /// The window's filter field ([`Self::with_filter`]): its query and the
    /// count the filtered column reports.
    pub fn filter(&self) -> Option<&SearchField> {
        self.filter.as_ref()
    }

    pub fn filter_mut(&mut self) -> Option<&mut SearchField> {
        self.filter.as_mut()
    }

    // --- state -------------------------------------------------------------

    /// The geometry of the last layout, in window coordinates.
    pub fn geometry(&self) -> &NavGeometry {
        &self.geometry
    }

    pub fn has_sidebar(&self) -> bool {
        self.inputs.borrow().has_sidebar
    }

    pub fn has_overview(&self) -> bool {
        self.inputs.borrow().has_overview
    }

    /// Whether the sidebar shows (docked and not hidden, or a drawer that
    /// is open, or the front push screen).
    pub fn is_sidebar_open(&self) -> bool {
        if !self.has_sidebar() {
            return false;
        }
        match self.geometry.sidebar {
            SidebarPlacement::Screen => self.front_pane() == NavPane::Sidebar,
            placement => self.inputs.borrow().sidebar.is_open(layout_of(placement)),
        }
    }

    /// Whether the inspector shows.
    pub fn is_inspector_open(&self) -> bool {
        let layout = match self.geometry.inspector {
            InspectorPlacement::None => return false,
            placement => inspector_layout_of(placement),
        };
        self.inputs.borrow().inspector.is_open(layout)
    }

    /// The front screen of the push stack (meaningful when it has more than
    /// one screen).
    pub fn front_pane(&self) -> NavPane {
        self.inputs.borrow().pane
    }

    /// Show or hide the sidebar, remembering the choice for this window size
    /// class. In [`NavMode::Push`] narrow, the sidebar is a screen: showing
    /// it goes back to it.
    pub fn toggle_sidebar(&mut self) {
        if !self.has_sidebar() {
            return;
        }
        if self.geometry.sidebar == SidebarPlacement::Screen {
            self.show(NavPane::Sidebar);
            return;
        }
        let layout = layout_of(self.geometry.sidebar);
        self.inputs.borrow_mut().sidebar.toggle(layout);
        self.after_flap_change();
    }

    pub fn open_sidebar(&mut self) {
        if self.has_sidebar() && !self.is_sidebar_open() {
            self.toggle_sidebar();
        }
    }

    pub fn close_sidebar(&mut self) {
        if self.has_sidebar()
            && self.is_sidebar_open()
            && self.geometry.sidebar != SidebarPlacement::Screen
        {
            self.toggle_sidebar();
        }
    }

    /// Show or hide the inspector, remembering the choice for this window
    /// size class (also while the app takes the inspector away and gives it
    /// back).
    pub fn toggle_inspector(&mut self) {
        let layout = match self.geometry.inspector {
            InspectorPlacement::None => return,
            placement => inspector_layout_of(placement),
        };
        self.inputs.borrow_mut().inspector.toggle(layout);
        self.after_flap_change();
    }

    pub fn open_inspector(&mut self) {
        if !self.is_inspector_open() {
            self.toggle_inspector();
        }
    }

    pub fn close_inspector(&mut self) {
        if self.is_inspector_open() {
            self.toggle_inspector();
        }
    }

    fn after_flap_change(&mut self) {
        if self.layout_theme.reduce_motion {
            self.fade = 0.0;
            self.last_tick.get_or_insert_with(crate::time::now);
        }
        self.animating = true;
        self.structure_dirty = true;
        self.update_toggle_names();
        self.publish_open_states();
        self.invalidate();
    }

    /// Write the sidebar's and inspector's openness to bound states.
    fn publish_open_states(&mut self) {
        let sidebar = self.is_sidebar_open();
        let inspector = self.is_inspector_open();
        if let Some(state) = self.sidebar_state.as_mut()
            && state.wanted.is_none()
        {
            state.binding.publish(&sidebar);
        }
        if let Some(state) = self.inspector_state.as_mut()
            && state.wanted.is_none()
        {
            state.binding.publish(&inspector);
        }
    }

    /// Bring the sidebar and inspector in line with bound states that
    /// changed, once the layout knows where they stand.
    fn apply_open_states(&mut self) {
        for sidebar in [true, false] {
            let slot = if sidebar {
                &mut self.sidebar_state
            } else {
                &mut self.inspector_state
            };
            let Some(state) = slot.as_mut() else {
                continue;
            };
            if let Some(value) = state.binding.poll() {
                state.wanted = Some(value);
            }
            let Some(wanted) = state.wanted else {
                continue;
            };
            let placed = if sidebar {
                self.geometry.sidebar != SidebarPlacement::None
            } else {
                self.geometry.inspector != InspectorPlacement::None
            };
            if !placed {
                continue;
            }
            if let Some(state) = if sidebar {
                self.sidebar_state.as_mut()
            } else {
                self.inspector_state.as_mut()
            } {
                state.wanted = None;
            }
            let open = if sidebar {
                self.is_sidebar_open()
            } else {
                self.is_inspector_open()
            };
            if open != wanted {
                if sidebar {
                    self.toggle_sidebar();
                } else {
                    self.toggle_inspector();
                }
            }
        }
    }

    /// Bring `pane` to the front of a narrow window's push stack. In a wide
    /// window every pane is already showing; this records which one a later
    /// collapse keeps in front.
    pub fn show(&mut self, pane: NavPane) {
        let pane = match pane {
            NavPane::Sidebar if !self.has_sidebar() => NavPane::Content,
            NavPane::Overview if !self.has_overview() => NavPane::Content,
            pane => pane,
        };
        if pane != NavPane::Sidebar {
            self.active_hint = Some(pane);
        }
        self.pane_chosen = true;
        {
            let mut inputs = self.inputs.borrow_mut();
            if inputs.pane == pane {
                return;
            }
            let stack = &self.geometry.stack;
            let from = inputs
                .push_pos
                .unwrap_or_else(|| f64_of_usize(stack_index(stack, inputs.pane)));
            inputs.pane = pane;
            inputs.push_from = Some(from);
            inputs.push_progress = 0.0;
            if self.layout_theme.reduce_motion || stack.is_empty() {
                inputs.push_pos = None;
                inputs.push_progress = 1.0;
            } else {
                self.last_tick.get_or_insert_with(crate::time::now);
            }
        }
        if let Some(binding) = self.pane_binding.as_mut() {
            binding.publish(&pane);
        }
        self.animating = true;
        self.structure_dirty = true;
        self.invalidate();
    }

    /// Go back one screen: the app's page stack first ([`Self::with_back`]),
    /// then a narrow window's push stack. Returns whether it did.
    pub fn back(&mut self) -> bool {
        if self.inputs.borrow().content_back
            && self.geometry.back_column == Some(Column::Content)
            && let Some(back) = self.content_back.as_mut()
        {
            (back.go_back)();
            self.refresh_content_back();
            return true;
        }
        self.pop_push_stack()
    }

    fn pop_push_stack(&mut self) -> bool {
        let stack = self.geometry.stack.clone();
        let front = stack_index(&stack, self.front_pane());
        let Some(previous) = front.checked_sub(1).and_then(|i| stack.get(i).copied()) else {
            return false;
        };
        self.show(previous);
        true
    }

    /// Ask the app's page stack whether it can go back.
    fn refresh_content_back(&mut self) {
        let Some(back) = self.content_back.as_ref() else {
            return;
        };
        let can = (back.can_go_back)();
        self.set_can_go_back(can);
    }

    /// Snap every animation to its end (deterministic shots and tests).
    pub fn snap(&mut self) {
        {
            let mut inputs = self.inputs.borrow_mut();
            let layout = layout_of(self.geometry.sidebar);
            let open = inputs.sidebar.is_open(layout);
            inputs.sidebar.snap(open);
            let layout = inspector_layout_of(self.geometry.inspector);
            let open = inputs.inspector.is_open(layout);
            inputs.inspector.snap(open);
            inputs.push_pos = None;
            inputs.push_from = None;
            inputs.push_progress = 1.0;
        }
        self.fade = 1.0;
        self.switch_fade = 1.0;
        self.switch_columns = [false; 4];
        self.animating = false;
        self.structure_dirty = true;
        self.invalidate();
    }

    fn update_toggle_names(&mut self) {
        let shortcut = HostShortcut::Sidebar
            .shortcut(self.conventions)
            .label(self.conventions);
        let sidebar_open = self.is_sidebar_open();
        let name = if sidebar_open {
            "Hide sidebar"
        } else {
            "Show sidebar"
        };
        self.toggle.set_accessible_name(name);
        self.toggle
            .set_tooltip(Some(format!("{name} ({shortcut})")));
        let name = if self.is_inspector_open() {
            "Hide inspector"
        } else {
            "Show inspector"
        };
        self.inspector_toggle.set_accessible_name(name);
        self.inspector_toggle.set_tooltip(Some(name.to_owned()));
    }

    // --- parts -------------------------------------------------------------

    fn part(&self, part: Part) -> Option<&dyn Widget> {
        match part {
            Part::Body(Column::Sidebar) => self.sidebar.as_deref(),
            Part::Body(Column::Overview) => self.overview.as_deref(),
            Part::Body(Column::Content) => Some(self.content.as_ref()),
            Part::Body(Column::Inspector) => self.inspector.as_deref(),
            Part::Rail => self.rail.as_deref(),
            Part::Title(column) => self
                .headers
                .get(column.index())
                .map(|header| &header.title as &dyn Widget),
            Part::Leading(column) => self
                .headers
                .get(column.index())
                .and_then(|header| header.leading.as_deref()),
            Part::Action(column, i) => self
                .headers
                .get(column.index())
                .and_then(|header| header.actions.get(i))
                .map(AsRef::as_ref),
            Part::More(column) => self
                .headers
                .get(column.index())
                .map(|header| &header.more as &dyn Widget),
            Part::Controls => self.controls.as_deref(),
            Part::Filter => self.filter.as_ref().map(|f| f as &dyn Widget),
            Part::Toggle => Some(&self.toggle),
            Part::Back => Some(&self.back),
            Part::InspectorToggle => Some(&self.inspector_toggle),
            Part::History(false) => Some(&self.history_open),
            Part::History(true) => Some(&self.history_close),
        }
    }

    fn part_mut(&mut self, part: Part) -> Option<&mut (dyn Widget + 'static)> {
        match part {
            Part::Body(Column::Sidebar) => self.sidebar.as_deref_mut(),
            Part::Body(Column::Overview) => self.overview.as_deref_mut(),
            Part::Body(Column::Content) => Some(self.content.as_mut()),
            Part::Body(Column::Inspector) => self.inspector.as_deref_mut(),
            Part::Rail => self.rail.as_deref_mut(),
            Part::Title(column) => self
                .headers
                .get_mut(column.index())
                .map(|header| &mut header.title as &mut dyn Widget),
            Part::Leading(column) => self
                .headers
                .get_mut(column.index())
                .and_then(|header| header.leading.as_deref_mut()),
            Part::Action(column, i) => self
                .headers
                .get_mut(column.index())
                .and_then(|header| header.actions.get_mut(i))
                .map(AsMut::as_mut),
            Part::More(column) => self
                .headers
                .get_mut(column.index())
                .map(|header| &mut header.more as &mut dyn Widget),
            Part::Controls => self.controls.as_deref_mut(),
            Part::Filter => self.filter.as_mut().map(|f| f as &mut dyn Widget),
            Part::Toggle => Some(&mut self.toggle),
            Part::Back => Some(&mut self.back),
            Part::InspectorToggle => Some(&mut self.inspector_toggle),
            Part::History(false) => Some(&mut self.history_open),
            Part::History(true) => Some(&mut self.history_close),
        }
    }

    /// Every element the shell owns, shown or not: sync, update and wake
    /// reach parts the shell is not showing.
    fn all_parts(&self) -> Vec<Part> {
        let mut parts = vec![
            Part::Body(Column::Overview),
            Part::Body(Column::Content),
            Part::Body(Column::Inspector),
            Part::Body(Column::Sidebar),
            Part::Rail,
        ];
        for column in [
            Column::Overview,
            Column::Content,
            Column::Inspector,
            Column::Sidebar,
        ] {
            let header = at(&self.headers, column);
            if header.leading.is_some() {
                parts.push(Part::Leading(column));
            }
            for i in 0..header.actions.len() {
                parts.push(Part::Action(column, i));
            }
            if !header.actions.is_empty() {
                parts.push(Part::More(column));
            }
        }
        parts.extend([
            Part::Controls,
            Part::Filter,
            Part::Toggle,
            Part::Back,
            Part::InspectorToggle,
            Part::History(false),
            Part::History(true),
        ]);
        parts
    }

    fn for_each_owned(&self, mut visit: impl FnMut(&dyn Widget)) {
        for part in self.all_parts() {
            if let Some(widget) = self.part(part) {
                visit(widget);
            }
        }
    }

    fn for_each_owned_mut(&mut self, mut visit: impl FnMut(&mut (dyn Widget + 'static))) {
        for part in self.all_parts() {
            if let Some(widget) = self.part_mut(part) {
                visit(widget);
            }
        }
    }

    /// How many of `column`'s actions stand in the row.
    fn shown_actions(&self, column: Column) -> usize {
        let count = at(&self.headers, column).actions.len();
        at(&self.inputs.borrow().fit.borrow(), column)
            .shown
            .map_or(count, |shown| shown.min(count))
    }

    /// A part the layout put away: an action in the More menu, or the More
    /// button while everything fits.
    fn part_hidden(&self, part: Part) -> bool {
        match part {
            Part::Action(column, i) => i >= self.shown_actions(column),
            Part::More(column) => {
                self.shown_actions(column) >= at(&self.headers, column).actions.len()
            }
            _ => false,
        }
    }

    /// A part of a push screen that is not in front: laid out while it
    /// slides, but it takes no input and is not exposed. A floating drawer
    /// is not modal, so nothing beside it is inert.
    fn part_inert(&self, part: Part) -> bool {
        let g = &self.geometry;
        // Both screens remain laid out during a push animation, but only
        // the chosen screen is operable. AT and keyboard routing must switch
        // immediately, without waiting for the outgoing pixels to slide away.
        let column = match part {
            Part::Body(column)
            | Part::Title(column)
            | Part::Leading(column)
            | Part::Action(column, _)
            | Part::More(column) => Some(column),
            Part::Rail | Part::Controls => Some(Column::Overview),
            Part::Filter => g.filter_column(&self.inputs.borrow()),
            _ => None,
        };
        if !g.stack.is_empty()
            && let Some(column) = column
        {
            // `side_by_side` describes only overview/content. A sidebar
            // can still push that pair as one screen, including when no
            // overview exists. Use the actual stack for admission.
            let pane = match column {
                Column::Sidebar => Some(NavPane::Sidebar),
                Column::Overview if g.side_by_side => Some(NavPane::Content),
                Column::Overview => Some(NavPane::Overview),
                Column::Content => Some(NavPane::Content),
                Column::Inspector => None,
            };
            let front = g
                .stack
                .get(stack_index(&g.stack, self.inputs.borrow().pane))
                .copied();
            if pane.is_some_and(|pane| g.stack.contains(&pane) && front != Some(pane)) {
                return true;
            }
        }
        false
    }

    /// What the host meets for `part`: the part, or a stand-in while its
    /// push screen is not in front, so a walk that pairs children with the
    /// tree's nodes never reaches an inert column.
    fn exposed(&self, part: Part) -> Option<&dyn Widget> {
        if self.part_inert(part) {
            return Some(&INERT_PART);
        }
        self.part(part)
    }

    /// The parts the host walks: those in the host tree once it is built,
    /// in its order (the host pairs them with the tree's nodes), and every
    /// owned part before the first build, when there is no tree for their
    /// order to disagree with. A part of a screen not in front answers as an
    /// [`InertPart`]; an action in the More menu stays, so its door is still
    /// published.
    fn exposed_parts(&self) -> Vec<Part> {
        if self.built {
            self.host_parts.clone()
        } else {
            let history = self.inputs.borrow().history.map(HistoryControl::is_open);
            self.all_parts()
                .into_iter()
                .filter(|part| self.part(*part).is_some())
                .filter(|part| match part {
                    Part::Toggle => self.has_sidebar(),
                    Part::InspectorToggle => self.inspector.is_some(),
                    Part::Back | Part::More(_) => false,
                    Part::History(open) => history == Some(*open),
                    _ => true,
                })
                .collect()
        }
    }

    /// Which column's theme a part draws with.
    fn part_column(&self, part: Part) -> Option<Column> {
        match part {
            Part::Body(column)
            | Part::Title(column)
            | Part::Leading(column)
            | Part::Action(column, _)
            | Part::More(column) => Some(column),
            Part::Rail | Part::Controls => Some(Column::Overview),
            Part::Filter => self.geometry.filter_column(&self.inputs.borrow()),
            Part::Toggle => self.geometry.toggle_column,
            Part::Back => self.geometry.back_column,
            Part::InspectorToggle => self.geometry.inspector_toggle_column,
            Part::History(_) => self.geometry.history_column,
        }
    }

    fn column_theme(&self, theme: &Theme, column: Option<Column>) -> Theme {
        match column {
            Some(Column::Sidebar) => match self.geometry.sidebar {
                SidebarPlacement::Drawer => theme.on_ground(crate::chrome::panel_ground(theme)),
                _ => theme.in_region(crate::surface::Region::Sidebar),
            },
            Some(Column::Inspector) => match self.geometry.inspector {
                InspectorPlacement::Drawer => theme.on_ground(crate::chrome::panel_ground(theme)),
                _ => theme.in_region(crate::surface::Region::Inspector),
            },
            Some(Column::Overview | Column::Content) | None => theme.clone(),
        }
    }

    fn take_requests(&mut self) -> bool {
        let mut acted = self.requests.toggle.replace(false);
        if acted {
            self.toggle_sidebar();
        }
        if self.requests.back.replace(false) {
            acted |= self.back();
        }
        if self.requests.inspector.replace(false) {
            self.toggle_inspector();
            acted = true;
        }
        acted
    }

    /// Open a More menu whose button was pressed, with the actions that did
    /// not fit.
    fn open_more_menus(&mut self) {
        let viewport = self.rect;
        for column in Column::ALL {
            let shown = self.shown_actions(column);
            let header = at_mut(&mut self.headers, column);
            if !header.more.take_open_request() {
                continue;
            }
            let items = header
                .actions
                .iter()
                .skip(shown)
                .map(|action| {
                    let node = action.accessibility();
                    let name = node
                        .as_ref()
                        .and_then(|node| node.name.clone())
                        .unwrap_or_default();
                    let mut item = crate::widget::context_menu::MenuItem::selection(name)
                        .with_enabled(node.as_ref().is_none_or(|node| !node.state.disabled));
                    if let Some(checked) = node.as_ref().and_then(|node| node.state.checked) {
                        item = item.with_checked(checked);
                    }
                    item
                })
                .collect();
            header.more.open(items, viewport);
            self.paint_dirty = true;
        }
    }

    /// Activate the action the person chose in a More menu: from the event
    /// that chose it, so its invocation leaves with that event.
    fn take_more_choices(
        &mut self,
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        let mut acted = false;
        for column in Column::ALL {
            let shown = self.shown_actions(column);
            let header = at_mut(&mut self.headers, column);
            if let Some(row) = header.more.take_choice()
                && let Some(action) = header.actions.get_mut(shown.saturating_add(row))
            {
                action.perform_action(&crate::a11y::Action::Activate, &[], state, path);
                acted = true;
            }
        }
        acted
    }

    /// Poll the bound content title and the app's page stack.
    fn poll_bindings(&mut self) {
        let mut titles = Vec::new();
        for column in Column::ALL {
            if let Some(binding) = at_mut(&mut self.headers, column).binding.as_mut()
                && let Some(title) = binding.poll()
            {
                titles.push((column, title));
            }
        }
        for (column, title) in titles {
            self.set_title(column, title);
        }
        self.refresh_content_back();
    }

    /// Carry the filter field's query to the column it filters.
    fn sync_filter(&mut self) {
        let Some(column) = self.geometry.filter_column(&self.inputs.borrow()) else {
            return;
        };
        let target = match column {
            Column::Sidebar => self.sidebar.as_deref_mut(),
            Column::Overview => self.overview.as_deref_mut(),
            Column::Content => Some(self.content.as_mut()),
            Column::Inspector => self.inspector.as_deref_mut(),
        };
        if let (Some(field), Some(target)) = (self.filter.as_mut(), target) {
            crate::widget::sync_filter(field, target);
        }
    }

    /// The person chose a destination in the sidebar or a row of the
    /// overview: a narrow window moves on to what they chose.
    fn follow_navigation(&mut self) -> bool {
        let from_sidebar = self
            .sidebar
            .as_deref_mut()
            .is_some_and(|sidebar| crate::widget::nav_list::take_navigation(sidebar));
        let from_rail = self
            .rail
            .as_deref_mut()
            .is_some_and(|rail| crate::widget::nav_list::take_navigation(rail));
        let from_overview = self
            .overview
            .as_deref_mut()
            .is_some_and(|overview| crate::widget::nav_list::take_navigation(overview));
        if from_sidebar {
            if self.geometry.sidebar == SidebarPlacement::Drawer && self.is_sidebar_open() {
                self.toggle_sidebar();
            }
            if self.geometry.sidebar == SidebarPlacement::Screen {
                let next = if self.has_overview() && !self.geometry.side_by_side {
                    NavPane::Overview
                } else {
                    NavPane::Content
                };
                self.show(next);
            }
        }
        if (from_overview || from_rail)
            && !self.geometry.stack.is_empty()
            && !self.geometry.side_by_side
        {
            self.show(NavPane::Content);
        }
        from_sidebar || from_rail || from_overview
    }

    /// Advance the slides by the time since the last frame.
    fn tick(&mut self, theme: &Theme) {
        let now = crate::time::now();
        // Unclamped: a stalled frame lands where the clock says, as every
        // wall-clock tween does.
        let dt = self
            .last_tick
            .replace(now)
            .map_or(0.0, |t| now.saturating_duration_since(t).as_secs_f64());
        let base = theme.tokens.motion.durations.base();
        let reduce = theme.reduce_motion;
        let sidebar_layout = layout_of(self.geometry.sidebar);
        let inspector_layout = inspector_layout_of(self.geometry.inspector);
        let stack = self.geometry.stack.clone();
        let mut moving = false;
        {
            let mut inputs = self.inputs.borrow_mut();
            if reduce {
                inputs.sidebar.settle(sidebar_layout);
                inputs.inspector.settle(inspector_layout);
            } else {
                moving |= inputs.sidebar.advance(sidebar_layout, now, base);
                moving |= inputs.inspector.advance(inspector_layout, now, base);
            }
            if stack.is_empty() || (inputs.push_progress >= 1.0 && inputs.push_pos.is_none()) {
                inputs.push_pos = None;
                inputs.push_progress = 1.0;
            } else {
                let target = f64_of_usize(stack_index(&stack, inputs.pane));
                if reduce || base.as_secs_f64() <= 0.0 {
                    inputs.push_progress = 1.0;
                } else if inputs.push_progress < 1.0 {
                    inputs.push_progress =
                        (inputs.push_progress + dt / base.as_secs_f64()).min(1.0);
                }
                let eased = crate::ease_out(inputs.push_progress);
                let from = inputs.push_from.unwrap_or(target);
                if inputs.push_progress >= 1.0 {
                    // Settled: the front screen's index, exactly.
                    inputs.push_pos = None;
                    inputs.push_from = None;
                } else {
                    inputs.push_pos = Some(from + (target - from) * eased);
                    moving = true;
                }
            }
        }
        if self.switch_fade < 1.0 {
            // A layout switch's fade: `base`, at most `fast` under reduced
            // motion.
            let span = theme.motion().fade(base).as_secs_f64();
            self.switch_fade = if span <= 0.0 {
                1.0
            } else {
                (self.switch_fade + dt / span).min(1.0)
            };
            if self.switch_fade >= 1.0 {
                self.switch_columns = [false; 4];
            }
            self.paint_dirty = true;
            moving |= self.switch_fade < 1.0;
        }
        if reduce && self.fade < 1.0 {
            let fast = theme.tokens.motion.durations.fast().as_secs_f64();
            self.fade = if fast <= 0.0 {
                1.0
            } else {
                (self.fade + dt / fast).min(1.0)
            };
            self.paint_dirty = true;
            self.animating = self.fade < 1.0 || moving;
            if !self.animating {
                self.last_tick = None;
            }
            return;
        }
        if moving {
            self.invalidate();
        } else if self.animating {
            // The last frame of a slide may hide a part.
            self.structure_dirty = true;
            self.invalidate();
        }
        if !moving {
            self.last_tick = None;
        }
        self.animating = moving;
    }

    fn refresh_geometry(&mut self) {
        let local = plan(&self.inputs.borrow(), self.rect.width(), self.rect.height());
        self.geometry = local.translated(Vec2::new(self.rect.x0, self.rect.y0));
    }

    /// Note a change of size class: the sidebar's choice for the new class
    /// applies, and a collapse keeps the pane the person worked in. An
    /// inspector the app took away keeps the choice made for it.
    fn reconcile_size_class(&mut self, before: &NavGeometry) {
        let layout = layout_of(self.geometry.sidebar);
        let stack = self.geometry.stack.clone();
        let previous = self.last_stack.replace(stack.clone());
        {
            let mut inputs = self.inputs.borrow_mut();
            inputs.sidebar.reconcile(layout);
            if self.geometry.inspector != InspectorPlacement::None {
                inputs
                    .inspector
                    .reconcile(inspector_layout_of(self.geometry.inspector));
            }
        }
        if previous.as_ref() != Some(&stack) {
            let switched = previous.is_some();
            // The first layout opens on the stack's first screen unless the
            // app chose one; a later collapse keeps the pane the person
            // worked in.
            // A later collapse lands where the keyboard is (a list the person
            // is arrowing through stays in front, focus with it), else on
            // the pane they last worked in.
            let keep = match (previous, self.pane_chosen) {
                // A new window opens on the first list of items: the
                // content's own list where it has one (a mail window on the
                // inbox, a Files window on the folder; Back goes to the
                // mailboxes or places), else the sidebar (Settings' list).
                (None, false) => {
                    let kind = self.inputs.borrow().kind;
                    if stack.contains(&NavPane::Overview) {
                        NavPane::Overview
                    } else if kind == ContentKind::List {
                        NavPane::Content
                    } else {
                        stack.first().copied().unwrap_or(NavPane::Content)
                    }
                }
                (None, true) => self.front_pane(),
                (Some(_), _) => self
                    .focused_pane()
                    .or(self.active_hint)
                    .unwrap_or(NavPane::Content),
            };
            let mut inputs = self.inputs.borrow_mut();
            inputs.pane = if stack.is_empty() || stack.contains(&keep) {
                keep
            } else {
                NavPane::Content
            };
            inputs.push_pos = None;
            inputs.push_from = None;
            inputs.push_progress = 1.0;
            drop(inputs);
            if switched {
                self.move_into_layout(before);
            }
            self.structure_dirty = true;
            self.update_toggle_names();
            self.invalidate();
        }
    }

    /// Carry the columns from where they stood (`before`, in the shell's
    /// own coordinates) into the layout a resize switched to, so nothing
    /// jumps under the person during a live resize (motion,
    /// `motion.durations.base`):
    ///
    /// - Narrowing into one column at a time, the screen in front slides
    ///   from where it stood beside the sidebar to fill the window, the
    ///   sidebar sliding out under the leading edge: the push stack's own
    ///   slide, started part-way.
    /// - Widening into a docked sidebar the person was not looking at, the
    ///   sidebar slides in from the leading edge and the content makes room.
    /// - A column that appears or moves where no slide carries it fades in.
    ///
    /// Under reduced motion nothing slides: every column that appeared or
    /// moved fades in over `motion.durations.fast`.
    fn move_into_layout(&mut self, before: &NavGeometry) {
        let (w, h) = (self.rect.width(), self.rect.height());
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let reduce = self.layout_theme.reduce_motion;
        // The new layout, settled on its front screen.
        let after = plan(&self.inputs.borrow(), w, h);
        let mut carried = [false; 4];
        if !reduce {
            let mut inputs = self.inputs.borrow_mut();
            let front = inputs.pane;
            let front_column = match front {
                NavPane::Sidebar => Column::Sidebar,
                NavPane::Overview => Column::Overview,
                NavPane::Content => Column::Content,
            };
            let was = before.column(front_column);
            let is = after.column(front_column);
            if after.stack.contains(&front)
                && was.width() > 0.0
                && is.width() > 0.0
                && (was.x0 - is.x0).abs() > 0.5
            {
                // Screen `i` stands at `(i - pos) * w` from its settled
                // place: start the slide where the front screen stood.
                let i = f64_of_usize(stack_index(&after.stack, front));
                let last = f64_of_usize(after.stack.len().saturating_sub(1));
                let from = (i - (was.x0 - is.x0) / w).clamp(0.0, last);
                inputs.push_from = Some(from);
                inputs.push_pos = Some(from);
                inputs.push_progress = 0.0;
                for (column, pane) in [
                    (Column::Sidebar, NavPane::Sidebar),
                    (Column::Overview, NavPane::Overview),
                    (Column::Content, NavPane::Content),
                ] {
                    carried[column.index()] = after.stack.contains(&pane);
                }
            }
            if before.sidebar != SidebarPlacement::Docked
                && after.sidebar == SidebarPlacement::Docked
                && before.sidebar_column.width() <= 0.0
            {
                inputs.sidebar.slide_in(SplitLayout::Docked);
                // The sidebar slides in and the columns beside it make
                // room as it does.
                carried = [true; 4];
            }
        }
        let mut faded = [false; 4];
        for column in Column::ALL {
            let (was, is) = (before.column(column), after.column(column));
            let appeared = is.width() > 0.0 && was.width() <= 0.0;
            let moved = is.width() > 0.0 && was.width() > 0.0 && (was.x0 - is.x0).abs() > 0.5;
            faded[column.index()] = !carried[column.index()] && (appeared || moved);
        }
        if faded.contains(&true) {
            self.switch_columns = faded;
            self.switch_fade = 0.0;
        }
        if carried.contains(&true) || faded.contains(&true) {
            self.last_tick.get_or_insert_with(crate::time::now);
            self.animating = true;
        }
    }

    /// The pane holding keyboard focus, if a list column holds it.
    fn focused_pane(&self) -> Option<NavPane> {
        fn holds_focus(widget: &dyn Widget) -> bool {
            if widget.is_focused() {
                return true;
            }
            let mut found = false;
            widget.for_each_child(&mut |child| found = found || holds_focus(child));
            found
        }
        if self.sidebar.as_deref().is_some_and(holds_focus) {
            Some(NavPane::Sidebar)
        } else if self.overview.as_deref().is_some_and(holds_focus) {
            Some(NavPane::Overview)
        } else {
            None
        }
    }

    /// Show each column's actions with their labels or their icons alone,
    /// as the layout decided.
    fn apply_fit(&mut self) {
        let fit = Rc::clone(&self.inputs.borrow().fit);
        let mut fit = fit.borrow_mut();
        for (slot, header) in fit.iter_mut().zip(&mut self.headers) {
            let shown = slot.shown.unwrap_or(header.actions.len());
            header.more.set_shown(shown < header.actions.len());
            for (i, action) in header.actions.iter_mut().enumerate() {
                if !slot.capable.get(i).copied().unwrap_or(false) {
                    continue;
                }
                let want = slot.compact;
                if slot.compact_now.get(i).copied() == Some(want) {
                    continue;
                }
                if let Some(button) = action
                    .as_any_mut()
                    .and_then(|any| any.downcast_mut::<Button>())
                {
                    button.set_compact_toolbar_presentation(want);
                    if let Some(now) = slot.compact_now.get_mut(i) {
                        *now = want;
                    }
                }
            }
        }
    }

    // --- painting ----------------------------------------------------------

    fn paint(&mut self, f: &mut Frame<'_>) {
        let g = self.geometry.clone();
        // Under reduced motion a sidebar or drawer that just appeared fades
        // in instead of sliding.
        let fade = unit_f32(self.fade);

        // Docked regions under their columns.
        if g.inspector == InspectorPlacement::Docked && g.inspector_column.width() > 0.0 {
            crate::surface::region(
                f.scene,
                f.transform,
                g.inspector_column,
                crate::surface::Region::Inspector,
                f.theme,
            );
        }
        let sidebar_region = matches!(
            g.sidebar,
            SidebarPlacement::Docked | SidebarPlacement::Screen
        ) && g.sidebar_column.width() > 0.0;

        if self.built_has(Part::Rail) && g.rail.width() > 0.0 {
            let local = self.column_theme(f.theme, Some(Column::Overview));
            f.draw(self.part_mut(Part::Rail), &local);
        }
        // Columns standing in the window, back to front.
        for column in [
            Column::Overview,
            Column::Content,
            Column::Inspector,
            Column::Sidebar,
        ] {
            if g.floating(column) {
                continue;
            }
            let faded = column == Column::Sidebar && sidebar_region && fade < 1.0;
            if faded {
                f.scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    fade,
                    f.transform,
                    &g.sidebar_column,
                );
            }
            if column == Column::Sidebar && sidebar_region {
                crate::surface::region(
                    f.scene,
                    f.transform,
                    g.sidebar_column,
                    crate::surface::Region::Sidebar,
                    f.theme,
                );
            }
            self.paint_column(f, column);
            if faded {
                f.scene.pop_layer();
            }
        }
        self.paint_rules(f.scene, f.transform, f.theme, &g);

        // Drawers over the columns, under the header row; the row's buttons
        // stay above everything. No scrim: a drawer is not modal, and the
        // content beside it stays as it is, under the drawer's shadow.
        if g.open_drawer().is_some() {
            let faded = fade < 1.0;
            if faded {
                f.scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    fade,
                    f.transform,
                    &self.rect,
                );
            }
            for column in [Column::Inspector, Column::Sidebar] {
                let rect = g.column(column);
                if !g.floating(column) || rect.width() <= 0.0 {
                    continue;
                }
                crate::widget::sidebar::paint_floating_drawer(f.scene, f.transform, rect, f.theme);
                self.paint_column(f, column);
            }
            if faded {
                f.scene.pop_layer();
            }
        }
        let history = self
            .inputs
            .borrow()
            .history
            .map(|control| Part::History(control.is_open()));
        for part in [Part::Back, Part::InspectorToggle, Part::Toggle]
            .into_iter()
            .chain(history)
        {
            self.paint_part(f, part);
        }
        self.paint_sync_status(f);
    }

    /// The sync status, in the header of the column History stands in (the
    /// one reaching the window's trailing edge): after its title and before
    /// its trailing controls, wherever the row has room.
    fn paint_sync_status(&mut self, f: &mut Frame<'_>) {
        self.status_slot = Rect::ZERO;
        let g = &self.geometry;
        let column = g.history_column.unwrap_or(Column::Content);
        let row = g.header(column);
        if row.width() <= 0.0 || row.height() <= 0.0 || g.column(column).width() <= 0.0 {
            self.sync_status
                .paint(f.scene, f.transform, f.text, f.theme, row, 0.0, 0.0);
            return;
        }
        let gap = f64::from(f.theme.tokens.spacing.s3);
        let in_row = |rect: Rect| {
            rect.width() > 0.0 && rect.y0 < row.y1 && rect.y1 > row.y0 && rect.x0 >= row.x0
        };
        let actions = at(&self.headers, column).actions.len();
        let mut trailing: Vec<Rect> = (0..actions)
            .map(|i| Part::Action(column, i))
            .chain([Part::More(column)])
            .filter(|part| self.built_has(*part) && !self.part_hidden(*part))
            .filter_map(|part| self.exposed(part).map(|widget| widget.get_rect()))
            .collect();
        trailing.extend([g.history, g.inspector_toggle, g.window_controls]);
        let x1 = trailing
            .into_iter()
            .filter(|rect| in_row(*rect))
            .map(|rect| rect.x0)
            .fold(row.x1, f64::min)
            - gap;
        let x0 = [
            Part::Title(column),
            Part::Leading(column),
            Part::Toggle,
            Part::Back,
        ]
        .into_iter()
        .filter(|part| self.built_has(*part) && !self.part_hidden(*part))
        .filter_map(|part| match part {
            Part::Toggle => Some(g.toggle),
            Part::Back => Some(g.back),
            _ => self.exposed(part).map(|widget| widget.get_rect()),
        })
        .filter(|rect| in_row(*rect))
        .map(|rect| rect.x1)
        .fold(row.x0, f64::max)
            + gap;
        self.sync_status
            .paint(f.scene, f.transform, f.text, f.theme, row, x0, x1);
        let said = self.sync_status.get_rect();
        let x1 = if said.is_zero_area() {
            x1
        } else {
            said.x0 - gap
        };
        if x1 > x0 {
            self.status_slot = Rect::new(x0, row.y0, x1, row.y1);
        }
    }

    /// Where a document's transient status goes in the header row that
    /// carries the sync status: after the title and before the trailing
    /// controls and the sync status, the row's height. Empty until the
    /// shell paints, and when the row has no room. A save status shown
    /// here covers none of the document.
    #[must_use]
    pub fn header_status_slot(&self) -> Rect {
        self.status_slot
    }

    /// Whether `part` is in the host tree this frame: only those were laid
    /// out, so only those paint.
    fn built_has(&self, part: Part) -> bool {
        self.host_parts.contains(&part)
    }

    fn paint_column(&mut self, f: &mut Frame<'_>, column: Column) {
        let rect = self.geometry.column(column);
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        let local = self.column_theme(f.theme, Some(column));
        let clip = rect.intersect(self.rect);
        // A column a layout switch brought in fades in.
        let alpha = if self.switch_columns[column.index()] {
            unit_f32(self.switch_fade)
        } else {
            1.0
        };
        f.scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            alpha,
            f.transform,
            &clip,
        );
        // The column's content first, then its header row: a row in the
        // window's bar stands over the content with its own ground.
        let mut under = vec![Part::Body(column)];
        if self.geometry.filter_column(&self.inputs.borrow()) == Some(column) {
            under.push(Part::Filter);
        }
        if column == Column::Overview {
            under.push(Part::Controls);
        }
        let mut over = vec![Part::Title(column), Part::Leading(column)];
        let actions = at(&self.headers, column).actions.len();
        over.extend((0..actions).map(|i| Part::Action(column, i)));
        over.push(Part::More(column));
        for part in under {
            if self.built_has(part) && !self.part_hidden(part) {
                f.draw(self.part_mut(part), &local);
            }
        }
        self.paint_bar_ground(f, column, &local);
        for part in over {
            if self.built_has(part) && !self.part_hidden(part) {
                f.draw(self.part_mut(part), &local);
            }
        }
        f.scene.pop_layer();
    }

    /// A header row in the window's bar stands over the column's content:
    /// it brings its own ground, rule and a soft shadow on the content.
    fn paint_bar_ground(&self, f: &mut Frame<'_>, column: Column, local: &Theme) {
        let header = self.geometry.header(column);
        if !self.inputs.borrow().header_in_window_bar()
            || header.width() <= 0.0
            || header.height() <= 0.0
            || self.geometry.floating(column)
        {
            return;
        }
        crate::surface::shadow(
            f.scene,
            f.transform,
            header,
            0.0,
            &local.tokens.elevation.floating,
        );
        let region = match column {
            Column::Sidebar => crate::surface::Region::Sidebar,
            Column::Inspector => crate::surface::Region::Inspector,
            Column::Overview | Column::Content => crate::surface::Region::Content,
        };
        crate::surface::region(f.scene, f.transform, header, region, local);
        crate::chrome::HeaderRowFace::in_row(header, self.inputs.borrow().metrics.row).paint_rule(
            f.scene,
            f.transform,
            local,
        );
    }

    fn paint_part(&mut self, f: &mut Frame<'_>, part: Part) {
        let rect = match part {
            Part::Toggle => self.geometry.toggle,
            Part::Back => self.geometry.back,
            Part::InspectorToggle => self.geometry.inspector_toggle,
            Part::History(_) => self.geometry.history,
            _ => return,
        };
        if rect.width() <= 0.0 || !self.built_has(part) {
            return;
        }
        let local = self.column_theme(f.theme, self.part_column(part));
        f.draw(self.part_mut(part), &local);
    }

    /// The hairline under the header row over the content columns, and the
    /// rule between the overview (or its rail) and the content. Regions are
    /// told apart by tone, not lines.
    fn paint_rules(&self, scene: &mut Scene, transform: Affine, theme: &Theme, g: &NavGeometry) {
        for column in [Column::Overview, Column::Content, Column::Inspector] {
            let header = g.header(column);
            // A row in the window's bar paints its rule with its ground.
            if header == Rect::ZERO
                || g.floating(column)
                || self.inputs.borrow().header_in_window_bar()
            {
                continue;
            }
            let local = self.column_theme(theme, Some(column));
            crate::chrome::HeaderRowFace::in_row(header, self.inputs.borrow().metrics.row)
                .paint_rule(scene, transform, &local);
        }
        let lead = if g.overview_column.width() > 0.0 {
            g.overview_column
        } else {
            g.rail
        };
        if g.side_by_side && lead.width() > 0.0 && g.content_column.width() > 0.0 {
            let x = lead.x1;
            let rule = Rect::new(x - 1.0, g.content_column.y0, x, lead.y1);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                crate::surface::edge_on(theme.tokens.colors.surfaces.content),
                None,
                &rule,
            );
        }
    }
}

fn downcast<'a, T: Widget + 'static>(
    slot: Option<&'a mut (dyn Widget + 'static)>,
) -> Option<&'a mut T> {
    slot.and_then(|widget| widget.as_any_mut())
        .and_then(|any| any.downcast_mut::<T>())
}

/// What one paint pass draws into.
struct Frame<'a> {
    scene: &'a mut Scene,
    transform: Affine,
    text: &'a mut TextRenderer,
    theme: &'a Theme,
    presence: &'a PresenceCtx,
}

impl Frame<'_> {
    /// Draw one of the shell's children with the theme of the ground it
    /// stands on.
    fn draw(&mut self, widget: Option<&mut (dyn Widget + 'static)>, theme: &Theme) {
        if let Some(widget) = widget {
            crate::widget::draw_child(
                widget,
                self.scene,
                self.transform,
                self.text,
                theme,
                self.presence,
            );
        }
    }
}

/// The layout policy the host runs: place every built part at the rect the
/// plan gives it, and lay each header row out from what its parts measure.
fn place_parts(
    inputs: &Inputs,
    ctx: &mut ManualContext<'_>,
) -> Result<Size<f32>, WidgetLayoutError> {
    let size = ctx.available();
    let g = plan(inputs, f64::from(size.width), f64::from(size.height));
    let mut placed: Vec<Option<Rect>> = vec![None; inputs.parts.len()];
    for (index, part) in inputs.parts.iter().enumerate() {
        let rect = match part {
            Part::Body(column) => Some(g.body(*column)),
            Part::Rail => Some(g.rail),
            Part::Filter => Some(g.filter),
            Part::Toggle => Some(g.toggle),
            Part::Back => Some(g.back),
            Part::InspectorToggle => Some(g.inspector_toggle),
            Part::History(_) => Some(g.history),
            Part::Controls => {
                let band = g.controls;
                if band.width() <= 0.0 {
                    Some(Rect::ZERO)
                } else {
                    // Its own width (the band's when it has none of its
                    // own), a control's height: the band is one control tall.
                    let measured = measure(ctx, index, Some(band.width()))?;
                    let w = if measured.0 > 0.0 {
                        measured.0.min(band.width())
                    } else {
                        band.width()
                    };
                    let h = if measured.1 > 0.0 {
                        measured.1.min(band.height())
                    } else {
                        band.height()
                    };
                    let y0 = band.center().y - h / 2.0;
                    Some(Rect::new(band.x0, y0, band.x0 + w, y0 + h))
                }
            }
            Part::Title(_) | Part::Leading(_) | Part::Action(..) | Part::More(_) => None,
        };
        if let (Some(slot), Some(rect)) = (placed.get_mut(index), rect) {
            *slot = Some(rect);
        }
    }
    for column in Column::ALL {
        place_header(inputs, &g, column, ctx, &mut placed)?;
    }
    for (index, rect) in placed.into_iter().enumerate() {
        let rect = rect.unwrap_or(Rect::ZERO);
        let rect = if rect.width() < 0.0 || rect.height() < 0.0 {
            Rect::ZERO
        } else {
            rect
        };
        ctx.place(index, rect)?;
    }
    Ok(size)
}

/// A part's natural size, within `width` when given.
///
/// The natural size comes first: a bounded proposal replaces the part's own
/// width with `auto`, so a control that states a fixed size and measures
/// nothing (a switch) would come back zero wide. Only a part wider than
/// `width` is measured again within it, so a label can wrap or shrink.
fn measure(
    ctx: &mut ManualContext<'_>,
    index: usize,
    width: Option<f64>,
) -> Result<(f64, f64), WidgetLayoutError> {
    let natural = ManualContext::measure(
        ctx,
        index,
        Size {
            width: MeasureConstraint::Intrinsic,
            height: MeasureConstraint::Intrinsic,
        },
    )?
    .size;
    let natural = (f64::from(natural.width), f64::from(natural.height));
    let Some(bound) = width.map(|w| w.max(0.0)) else {
        return Ok(natural);
    };
    if natural.0 <= bound {
        return Ok(natural);
    }
    let proposal = Size {
        width: MeasureConstraint::AtMost(f32_of_f64(bound)),
        height: MeasureConstraint::Intrinsic,
    };
    let measured = ManualContext::measure(ctx, index, proposal)?.size;
    Ok((f64::from(measured.width), f64::from(measured.height)))
}

/// Lay out one header row: the title at its measured width after the
/// leading buttons, the actions trailing (labelled buttons collapsing to
/// their icons, then the rest moving into the More menu when they do not
/// fit), and the leading fill between them.
#[allow(
    clippy::too_many_lines,
    reason = "one row's layout is one ordered decision: title, actions, overflow, leading"
)]
fn place_header(
    inputs: &Inputs,
    g: &NavGeometry,
    column: Column,
    ctx: &mut ManualContext<'_>,
    placed: &mut [Option<Rect>],
) -> Result<(), WidgetLayoutError> {
    let header = g.header(column);
    let ci = column.index();
    let index_of = |part: Part| inputs.parts.iter().position(|p| *p == part);
    if header == Rect::ZERO || header.width() <= 0.0 {
        return Ok(());
    }
    let m = &inputs.metrics;
    let cy = header.center().y;
    let lead = g.lead_end(column, m);
    let trail = g.trail_start(column, m).max(lead);
    let avail = trail - lead;
    let gap = m.gap;
    let centred = |x0: f64, w: f64, h: f64| -> Rect {
        let h = h.min(header.height()).max(0.0);
        Rect::new(x0, cy - h / 2.0, x0 + w.max(0.0), cy + h / 2.0)
    };

    // The actions, at their natural sizes.
    let count = inputs.actions.get(ci).copied().unwrap_or(0);
    let mut items: Vec<(usize, f64, f64)> = Vec::with_capacity(count);
    for i in 0..count {
        if let Some(index) = index_of(Part::Action(column, i)) {
            let (w, h) = measure(ctx, index, Some(avail))?;
            items.push((index, w, h));
        }
    }
    let fit_cell = Rc::clone(&inputs.fit);
    let mut fits = fit_cell.borrow_mut();
    let fit = at_mut(&mut fits, column);
    if fit.capable.len() != items.len() {
        fit.capable.resize(items.len(), false);
        fit.compact_now.resize(items.len(), false);
        fit.full.resize(items.len(), None);
    }
    let mut full = Vec::with_capacity(items.len());
    let mut compact = Vec::with_capacity(items.len());
    for (i, (_, w, h)) in items.iter().enumerate() {
        let capable = fit.capable.get(i).copied().unwrap_or(false);
        let now = fit.compact_now.get(i).copied().unwrap_or(false);
        if capable && now {
            full.push(fit.full.get(i).copied().flatten().unwrap_or(*w));
            compact.push(*w);
        } else {
            if let Some(slot) = fit.full.get_mut(i) {
                *slot = Some(*w);
            }
            full.push(*w);
            compact.push(if capable { w.min(*h) } else { *w });
        }
    }
    let run = |widths: &[f64]| -> f64 {
        if widths.is_empty() {
            0.0
        } else {
            widths.iter().sum::<f64>() + gap * f64_of_usize(widths.len().saturating_sub(1))
        }
    };
    let reserve = run(&compact);

    // The title: its measured width, never pushing the actions past their
    // icons, and keeping a third of the row however many actions there are.
    let mut start = lead;
    if let Some(index) = index_of(Part::Title(column)) {
        let (tw, th) = measure(ctx, index, None)?;
        let room = if reserve > 0.0 {
            avail - reserve - gap
        } else {
            avail
        };
        let width = tw.min(room.max(tw.min(avail / 3.0))).clamp(0.0, avail);
        if let Some(slot) = placed.get_mut(index) {
            *slot = Some(centred(lead, width, th));
        }
        if width > 0.0 {
            start = lead + width + gap;
        }
    }
    let band = (trail - start).max(0.0);

    // Labels, icons, or the More menu.
    let more = index_of(Part::More(column));
    let (use_compact, shown) = if run(&full) <= band {
        (false, items.len())
    } else if run(&compact) <= band {
        (true, items.len())
    } else {
        let more_w = match more {
            Some(index) => measure(ctx, index, None)?.0,
            None => 0.0,
        };
        let mut k = items.len();
        while k > 0 {
            let used = run(compact.get(..k).unwrap_or(&[])) + gap + more_w;
            if used <= band {
                break;
            }
            k = k.saturating_sub(1);
        }
        (true, k)
    };
    fit.compact = use_compact;
    fit.shown = Some(shown);
    let widths = if use_compact { &compact } else { &full };
    let overflow = shown < items.len();
    let more_size = match (overflow, more) {
        (true, Some(index)) => Some((index, measure(ctx, index, None)?)),
        _ => None,
    };
    let mut total = run(widths.get(..shown).unwrap_or(&[]));
    if let Some((_, (mw, _))) = more_size {
        total += if shown > 0 { gap + mw } else { mw };
    }
    let mut x = (trail - total).max(start.min(trail));
    let actions_x0 = if total > 0.0 { x } else { trail };
    for (k, (index, _, h)) in items.iter().enumerate() {
        let rect = if k < shown {
            let w = widths.get(k).copied().unwrap_or(0.0);
            let rect = centred(x, w, *h);
            x += w + gap;
            rect
        } else {
            Rect::ZERO
        };
        if let Some(slot) = placed.get_mut(*index) {
            *slot = Some(rect);
        }
    }
    if let Some(index) = more {
        let rect = match more_size {
            Some((_, (mw, mh))) => centred(x, mw, mh),
            None => Rect::ZERO,
        };
        if let Some(slot) = placed.get_mut(index) {
            *slot = Some(rect);
        }
    }
    drop(fits);

    // The leading fill: its own width, or what is left when it needs more.
    if let Some(index) = index_of(Part::Leading(column)) {
        let end = if total > 0.0 { actions_x0 - gap } else { trail };
        let room = (end - start).max(0.0);
        let (lw, lh) = measure(ctx, index, Some(room))?;
        if let Some(slot) = placed.get_mut(index) {
            *slot = Some(centred(start, lw.min(room), lh));
        }
    }
    Ok(())
}

impl Widget for NavShell {
    crate::provides!(
        dyn crate::widget::controls::ActionControls,
        dyn WindowHeader
    );

    fn set_platform_conventions(&mut self, conventions: PlatformConventions) {
        self.conventions = conventions;
        self.update_toggle_names();
        self.for_each_owned_mut(|child| child.set_platform_conventions(conventions));
    }

    fn set_interaction_environment(
        &mut self,
        environment: crate::interaction::InteractionEnvironment,
    ) {
        self.for_each_owned_mut(|child| child.set_interaction_environment(environment));
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        let next = self.pane_binding.as_mut().and_then(LocalBinding::poll);
        if let Some(pane) = next {
            self.show(pane);
        }
        self.poll_bindings();
        if self.built {
            self.apply_open_states();
        }
        self.take_requests();
        self.open_more_menus();
        self.follow_navigation();
        self.for_each_owned_mut(|child| child.sync(state, path));
        self.sync_filter();
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let mut changed = self.take_requests();
        self.for_each_owned_mut(|child| changed |= child.update(state, path));
        changed |= self.follow_navigation();
        changed || self.animating
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The host is building a new tree: what waited for it goes in now.
        self.apply_pending();
        self.slots_changed();
        self.layout_theme = theme.clone();
        let size = if self.rect.width() > 0.0 {
            (self.rect.width(), self.rect.height())
        } else {
            // Before the first layout, assume a regular window.
            (
                f64::from(theme.tokens.metrics.compact_breakpoint) * 4.0,
                f64::from(theme.tokens.metrics.compact_breakpoint),
            )
        };
        let parts = {
            let mut inputs = self.inputs.borrow_mut();
            inputs.metrics = Metrics::resolve(theme);
            let g = plan(&inputs, size.0, size.1);
            let parts = g.visible_parts(&inputs);
            inputs.parts.clone_from(&parts);
            parts
        };
        self.update_toggle_names();
        let mut nodes = Vec::with_capacity(parts.len());
        for part in &parts {
            if let Some(widget) = self.part_mut(*part) {
                nodes.push(widget.build_layout(taffy, theme, node_map));
            }
        }
        self.baseline = nodes
            .iter()
            .map(|&node| (node, taffy.style(node).cloned().unwrap_or_default()))
            .collect();
        self.host_parts = parts;
        self.built = true;
        self.structure_dirty = false;
        self.invalidation.invalidate();
        let style = Style {
            size: Self::region_size(),
            ..Default::default()
        };
        taffy.widget_node(style, &nodes, node_map, self)
    }

    fn manual_layout_policy(&self) -> Option<ManualPolicy> {
        Some(ManualPolicy::for_children(
            Rc::clone(&self.policy),
            Self::region_size(),
            self.baseline.clone(),
            self.invalidation.clone(),
        ))
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        // The layout as it stood, in the shell's own coordinates: a switch
        // of layout moves on from there.
        let before = self
            .geometry
            .translated(Vec2::new(-self.rect.x0, -self.rect.y0));
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        self.refresh_geometry();
        self.reconcile_size_class(&before);
        self.refresh_geometry();
        // The side of each threshold this layout stands on: the next one
        // switches back only past the hysteresis band.
        self.inputs.borrow_mut().held = Held::of(&self.geometry);
        let child_origin = Point::new(self.rect.x0, self.rect.y0);
        let child_nodes = taffy.children(node).unwrap_or_default();
        let parts = self.host_parts.clone();
        for (part, child) in parts.iter().zip(child_nodes) {
            if let Some(widget) = self.part_mut(*part) {
                widget.apply_layout(taffy, child, child_origin);
            }
        }
        self.apply_fit();
        self.invalidation.acknowledge_applied();
        self.layout_dirty = false;
        self.apply_open_states();
        self.publish_open_states();
        let visible = self.geometry.visible_parts(&self.inputs.borrow());
        if visible != self.host_parts || !self.pending.is_empty() {
            self.structure_dirty = true;
        }
    }

    fn layout_needs_measure(&self) -> bool {
        let mut dirty = self.invalidation.is_pending();
        // Hidden headers and controls may never have been measured. They
        // must not invalidate an unrelated visible pane on every scroll.
        // Revealing a part changes host_parts and invalidates the shell.
        for part in &self.host_parts {
            if let Some(child) = self.part(*part) {
                dirty |= child.layout_needs_measure();
            }
        }
        dirty
    }

    fn is_layout_dirty(&self) -> bool {
        let mut dirty = self.layout_dirty || self.invalidation.is_pending();
        self.for_each_owned(|child| dirty |= child.is_layout_dirty());
        dirty
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.paint(&mut Frame {
            scene,
            transform,
            text: text_renderer,
            theme,
            presence,
        });
        self.tick(theme);
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        for child in self.children_mut() {
            child.draw_overlay(scene, transform, text_renderer, theme, presence);
        }
        self.toasts.advance(crate::time::now());
        let window = presence.viewport.get().unwrap_or(self.rect);
        self.toasts
            .place_bottom_right(window, f64::from(theme.tokens.spacing.s6));
        let mut painted = Scene::new();
        self.toasts
            .draw(&mut painted, transform, text_renderer, theme, presence);
        scene.append(&painted, None);
        self.toasts_scene = painted;
        self.toasts_painted = self.toasts.painted_bounds(theme);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if !self.toasts.is_empty() {
            let before = (self.toasts.len(), self.toasts.is_hovered());
            self.toasts.handle_event(event, ctx);
            // The dismiss control's hover shows too.
            self.toasts_dirty |= *ctx.handled
                || before != (self.toasts.len(), self.toasts.is_hovered())
                || matches!(event, WindowEvent::CursorMoved { .. });
            if *ctx.handled {
                return;
            }
        }
        if self.handle_own_event(event, ctx) {
            *ctx.handled = true;
            return;
        }
        self.route_event_to_children(event, ctx);
        self.take_requests();
        self.take_more_choices(&mut ctx.state, ctx.path);
        self.open_more_menus();
        self.follow_navigation();
        self.sync_filter();
    }

    fn children(&self) -> Vec<&dyn Widget> {
        let mut children: Vec<&dyn Widget> = self
            .exposed_parts()
            .iter()
            .filter_map(|part| self.exposed(*part))
            .collect();
        if self.sync_status.text().is_some() {
            children.push(&self.sync_status);
        }
        if !self.toasts.is_empty() {
            children.push(&self.toasts);
        }
        children
    }

    // A showing notice is the last child, so assistive technology announces
    // it as a live region.
    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        for part in self.exposed_parts() {
            if let Some(widget) = self.exposed(part) {
                visitor(widget);
            }
        }
        if self.sync_status.text().is_some() {
            visitor(&self.sync_status);
        }
        if !self.toasts.is_empty() {
            visitor(&self.toasts);
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let parts = self.exposed_parts();
        let inert: Vec<bool> = parts.iter().map(|part| self.part_inert(*part)).collect();
        self.inert_parts.resize_with(parts.len(), || InertPart);
        let NavShell {
            sidebar,
            overview,
            content,
            inspector,
            rail,
            filter,
            controls,
            headers,
            toggle,
            back,
            inspector_toggle,
            history_open,
            history_close,
            inert_parts,
            toasts,
            sync_status,
            ..
        } = self;
        let mut stubs = inert_parts.iter_mut();
        let mut bodies: [Option<&mut (dyn Widget + 'static)>; 4] = [
            sidebar.as_deref_mut(),
            overview.as_deref_mut(),
            Some(content.as_mut()),
            inspector.as_deref_mut(),
        ];
        let mut rail = rail.as_deref_mut();
        let mut filter = filter.as_mut().map(|f| f as &mut dyn Widget);
        let mut controls = controls.as_deref_mut();
        let mut titles: Vec<Option<&mut dyn Widget>> = Vec::with_capacity(4);
        let mut leadings: Vec<Option<&mut (dyn Widget + 'static)>> = Vec::with_capacity(4);
        let mut mores: Vec<Option<&mut dyn Widget>> = Vec::with_capacity(4);
        let mut actions: Vec<Vec<Option<&mut (dyn Widget + 'static)>>> = Vec::with_capacity(4);
        for header in headers.iter_mut() {
            let Header {
                title,
                actions: list,
                more,
                leading,
                ..
            } = header;
            titles.push(Some(title as &mut dyn Widget));
            leadings.push(leading.as_deref_mut());
            mores.push(Some(more as &mut dyn Widget));
            actions.push(list.iter_mut().map(|a| Some(a.as_mut())).collect());
        }
        let mut toggle = Some(toggle as &mut dyn Widget);
        let mut back = Some(back as &mut dyn Widget);
        let mut inspector_toggle = Some(inspector_toggle as &mut dyn Widget);
        let mut history_open = Some(history_open as &mut dyn Widget);
        let mut history_close = Some(history_close as &mut dyn Widget);
        let mut out: Vec<&mut (dyn Widget + '_)> = Vec::with_capacity(parts.len());
        for (part, inert) in parts.into_iter().zip(inert) {
            let stub = stubs.next();
            if inert {
                if let Some(stub) = stub {
                    out.push(stub);
                }
                continue;
            }
            let next: Option<&mut (dyn Widget + '_)> = match part {
                Part::Body(column) => bodies
                    .get_mut(column.index())
                    .and_then(Option::take)
                    .map(|w| w as &mut dyn Widget),
                Part::Rail => rail.take().map(|w| w as &mut dyn Widget),
                Part::Title(column) => titles.get_mut(column.index()).and_then(Option::take),
                Part::Leading(column) => leadings
                    .get_mut(column.index())
                    .and_then(Option::take)
                    .map(|w| w as &mut dyn Widget),
                Part::Action(column, i) => actions
                    .get_mut(column.index())
                    .and_then(|list| list.get_mut(i))
                    .and_then(Option::take)
                    .map(|w| w as &mut dyn Widget),
                Part::More(column) => mores.get_mut(column.index()).and_then(Option::take),
                Part::Controls => controls.take().map(|w| w as &mut dyn Widget),
                Part::Filter => filter.take(),
                Part::Toggle => toggle.take(),
                Part::Back => back.take(),
                Part::InspectorToggle => inspector_toggle.take(),
                Part::History(false) => history_open.take(),
                Part::History(true) => history_close.take(),
            };
            if let Some(widget) = next {
                out.push(widget);
            }
        }
        if sync_status.text().is_some() {
            out.push(sync_status);
        }
        if !toasts.is_empty() {
            out.push(toasts);
        }
        out
    }

    fn name(&self) -> &'static str {
        "NavShell"
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn test_id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn minimum_window_width(&self) -> Option<f64> {
        Some(self.minimum_width())
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        (self.resize.is_some() || self.geometry.resize_handle.contains(point))
            .then_some(winit::window::CursorIcon::ColResize)
    }

    /// The window's group: keyed by the shell's id, so what stands under a
    /// titled column keeps its identity on the agent surface while titles
    /// come and go. The column titles are its headings (the title parts).
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        let key = self.id.as_deref().unwrap_or("nav-shell");
        Some(AccessNode::new(Role::Group).with_semantic_key(format!("{key}.window")))
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        let mut earliest = self.toasts.next_wake_deadline();
        self.for_each_owned(|child| {
            if let Some(deadline) = child.next_wake_deadline() {
                earliest = Some(earliest.map_or(deadline, |old| old.min(deadline)));
            }
        });
        earliest
    }

    fn needs_animation_frame(&self) -> bool {
        // Hidden drawers retain their controls, but their entrance animations
        // cannot advance until the drawer is painted. Only painted parts ask
        // the host for frames; background wakes still visit every owned part.
        self.animating
            || self.fade < 1.0
            || self
                .host_parts
                .iter()
                .any(|part| self.part(*part).is_some_and(Widget::needs_animation_frame))
    }

    fn needs_idle_sync(&self) -> bool {
        let mut needed = self.animating
            || self.content_back.is_some()
            || self.headers.iter().any(|header| header.binding.is_some())
            || self.sidebar_state.is_some()
            || self.inspector_state.is_some();
        self.for_each_owned(|child| needed |= child.needs_idle_sync());
        needed
    }

    fn needs_redraw(&self) -> bool {
        self.paint_dirty
            || self.toasts_dirty
            || self.toasts_expired()
            || self.animating
            || self.fade < 1.0
            || self
                .host_parts
                .iter()
                .any(|part| self.part(*part).is_some_and(Widget::needs_redraw))
    }

    fn mark_drawn(&mut self) {
        self.paint_dirty = false;
        self.toasts_dirty = false;
        self.toasts_drawn = self.toasts_painted;
        self.toasts_drawn_scene.clone_from(&self.toasts_scene);
        self.for_each_owned_mut(|child| child.mark_drawn());
        // The shell paints its titles and notices itself, so they learn
        // here what it painted: as children they would otherwise ask for
        // a frame forever.
        for header in &mut self.headers {
            header.title.mark_drawn();
        }
        self.toasts.mark_drawn();
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        // The shell's own geometry, materials and transitions can change
        // outside a child's box. A settled shell can preserve local control
        // damage rather than rasterising every pane for a hover.
        if self.paint_dirty || self.animating || self.fade < 1.0 {
            regions.push(self.rect);
        } else {
            self.for_each_owned(|child| child.collect_damage(regions));
            // The notices where they were and are, shadows included.
            if self.toasts_changed() {
                regions.extend(
                    [self.toasts_drawn, self.toasts_painted]
                        .into_iter()
                        .flatten(),
                );
            }
        }
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.take_requests();
        let mut dirty = std::mem::take(&mut self.structure_dirty);
        self.for_each_owned_mut(|child| dirty |= child.take_structure_dirty());
        dirty
    }

    /// A part whose overlay holds the pointer (a popover a header button
    /// owns, an open More menu) first; then the default.
    fn modal_capture(&self) -> Option<Rect> {
        let mut capture = None;
        for part in self.exposed_parts() {
            if capture.is_none()
                && let Some(widget) = self.exposed(part)
            {
                capture = widget.modal_capture();
            }
        }
        capture
    }
}

/// Controls in a column the shell is not showing (a hidden sidebar, a
/// closed drawer, a screen pushed away) still belong to the window: custody,
/// invocations and availability reach every part the shell owns.
impl WindowHeader for NavShell {
    fn set_history_control(&mut self, control: Option<HistoryControl>) {
        if self.inputs.borrow().history == control {
            return;
        }
        self.inputs.borrow_mut().history = control;
        self.structure_dirty = true;
        self.invalidate();
    }

    fn set_sync_status(&mut self, status: Option<&str>) -> bool {
        if self.sync_status.set(status) {
            self.paint_dirty = true;
            self.invalidation.invalidate();
        }
        true
    }

    fn set_window_bar(&mut self, bar: Option<crate::widget::WindowBar>) -> bool {
        let controls = bar.map(|bar| bar.controls_width);
        let revealed = bar.is_some_and(|bar| bar.revealed);
        let top = bar.map_or(0.0, |bar| bar.top_inset);
        let relayout = {
            let mut inputs = self.inputs.borrow_mut();
            let relayout = inputs.window_controls != controls
                || (inputs.header_in_bar
                    && (inputs.bar_revealed != revealed
                        || (inputs.bar_top - top).abs() > f64::EPSILON));
            inputs.window_controls = controls;
            inputs.bar_revealed = revealed;
            inputs.bar_top = top;
            relayout
        };
        if relayout {
            self.structure_dirty = true;
            self.invalidate();
        }
        true
    }

    fn window_bar_slot(&self) -> Option<crate::widget::WindowBarSlot> {
        let g = &self.geometry;
        (g.window_controls.width() > 0.0).then(|| crate::widget::WindowBarSlot {
            row: g.window_row,
            controls: g.window_controls,
            hides_with_bar: self.inputs.borrow().header_in_bar,
        })
    }
}

impl crate::widget::controls::ActionControls for NavShell {
    fn action_control_bindings(&self) -> Vec<crate::widget::ControlBindingMetadata> {
        crate::widget::controls::children_action_control_bindings(self)
    }
    fn install_control_custody(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_custody::CustodyAttachment,
    ) {
        self.for_each_owned_mut(|child| {
            crate::widget::controls::install_control_custody(child, domain, attachment);
        });
    }

    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        let mut taken = None;
        self.for_each_owned_mut(|child| {
            if taken.is_none() {
                taken = crate::widget::controls::take_control_invocation(child);
            }
        });
        taken
    }

    fn on_control_invocation_result(
        &mut self,
        address: &tilo_actions::ActionAddress,
        result: &Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError>,
    ) {
        self.for_each_owned_mut(|child| {
            if !child.has_separate_document_owner() {
                crate::widget::controls::on_control_invocation_result(child, address, result);
            }
        });
    }

    fn has_action_control_bindings(&self) -> bool {
        crate::widget::controls::children_have_action_control_bindings(self)
    }

    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        self.for_each_owned_mut(|child| {
            crate::widget::controls::refresh_control_action_availability(child, allowed);
        });
    }

    fn has_pending_control_invocation(&self) -> bool {
        let mut pending = false;
        self.for_each_owned(|child| {
            pending |= crate::widget::controls::has_pending_control_invocation(child);
        });
        pending
    }
}

impl NavShell {
    /// The shell fills the window it is given.
    fn region_size() -> Size<taffy::Dimension> {
        Size::from_percent(1.0, 1.0)
    }

    /// What the shell itself answers: the sidebar shortcut, Escape, Ctrl+F,
    /// a scrim press, and the resize drag. Returns whether it consumed
    /// `event`.
    fn handle_own_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) -> bool {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = ctx.logical(position);
                if let Some(resize) = self.resize {
                    self.drag_sidebar_to(resize, self.last_cursor.x);
                    return true;
                }
                false
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                if self.modal_capture().is_some() {
                    return false;
                }
                self.handle_key(logical_key, ctx)
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if self
                    .modal_capture()
                    .is_some_and(|rect| rect.contains(self.last_cursor))
                {
                    return false;
                }
                self.handle_press()
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if self.resize.take().is_some() {
                    self.structure_dirty = true;
                    self.invalidate();
                    return true;
                }
                false
            }
            _ => false,
        }
    }

    fn handle_key(&mut self, key: &Key, ctx: &mut crate::widget::EventCtx<'_>) -> bool {
        let modifiers = ctx.modifiers.state();
        let command = self.conventions.command_modifier_down(modifiers);
        if command && let Key::Character(c) = key {
            if HostShortcut::resolve(self.conventions, c, modifiers.shift_key())
                == Some(HostShortcut::Sidebar)
                && self.has_sidebar()
            {
                self.toggle_sidebar();
                return true;
            }
            if c.eq_ignore_ascii_case("f") && !modifiers.shift_key() {
                let index = self
                    .host_parts
                    .iter()
                    .position(|part| *part == Part::Filter);
                if let Some(node) = index.and_then(|i| {
                    ctx.taffy
                        .children(ctx.node_id)
                        .ok()
                        .and_then(|children| children.get(i).copied())
                }) {
                    if self.geometry.sidebar == SidebarPlacement::Drawer && !self.is_sidebar_open()
                    {
                        self.toggle_sidebar();
                    }
                    *ctx.focus_target = Some(node);
                    return true;
                }
            }
        }
        if *key == Key::Named(NamedKey::Escape) {
            if self.geometry.sidebar == SidebarPlacement::Drawer && self.is_sidebar_open() {
                self.toggle_sidebar();
                return true;
            }
            if self.geometry.inspector == InspectorPlacement::Drawer && self.is_inspector_open() {
                self.toggle_inspector();
                return true;
            }
            if self.pop_push_stack() {
                return true;
            }
        }
        false
    }

    fn handle_press(&mut self) -> bool {
        let p = self.last_cursor;
        let g = &self.geometry;
        if g.resize_handle.contains(p) {
            self.resize = Some(Resize {
                start_x: p.x,
                start_width: g.sidebar_column.width(),
            });
            return true;
        }
        // A press on the content beside an open drawer puts the drawer away
        // (and does nothing else); the header row stays live.
        if let Some(column) = g.open_drawer()
            && !g.column(column).contains(p)
            && !g.window_row_contains(p)
        {
            match column {
                Column::Sidebar => self.toggle_sidebar(),
                _ => self.toggle_inspector(),
            }
            return true;
        }
        // Remember which content pane the person works in.
        if g.content_column.contains(p) {
            self.active_hint = Some(NavPane::Content);
        } else if g.overview_column.contains(p) || g.rail.contains(p) {
            self.active_hint = Some(NavPane::Overview);
        }
        false
    }

    fn drag_sidebar_to(&mut self, resize: Resize, x: f64) {
        let width = resize.start_width + (x - resize.start_x);
        let (min, scale) = {
            let inputs = self.inputs.borrow();
            (inputs.metrics.sidebar_min, inputs.metrics.scale)
        };
        if width < min * 0.75 {
            // Dragged past its minimum: the sidebar hides.
            if self.is_sidebar_open() {
                let mut inputs = self.inputs.borrow_mut();
                inputs.sidebar.snap(false);
            }
        } else {
            let mut inputs = self.inputs.borrow_mut();
            if !inputs.sidebar.is_open(SplitLayout::Docked) {
                inputs.sidebar.snap(true);
            }
            inputs.user_sidebar = Some(width.max(min) / scale.max(f64::EPSILON));
        }
        self.update_toggle_names();
        self.publish_open_states();
        self.structure_dirty = true;
        self.invalidate();
    }

    /// The rect a pointer event over `part` lands in.
    fn part_area(&self, part: Part) -> Rect {
        let g = &self.geometry;
        match part {
            Part::Toggle => g.toggle,
            Part::Back => g.back,
            Part::InspectorToggle => g.inspector_toggle,
            Part::History(_) => g.history,
            Part::Filter => g.filter,
            Part::Controls => g.controls,
            Part::Rail => g.rail,
            Part::Title(column)
            | Part::Leading(column)
            | Part::Action(column, _)
            | Part::More(column) => g.header(column),
            Part::Body(column) => g.body(column).union(g.column(column)),
        }
    }

    /// Route `event` to the live parts, front to back. A part whose overlay
    /// holds the pointer (a popover a header button owns) hears pointer
    /// events first; then pointer events go to the part under the pointer,
    /// so a release or a wheel lands where it happened and never on a column
    /// beside it. Parts under a drawer's scrim and actions in the More menu
    /// hear nothing.
    fn route_event_to_children(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        let Ok(child_nodes) = ctx.taffy.children(ctx.node_id) else {
            return;
        };
        let pointer = matches!(
            event,
            WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. }
                | WindowEvent::TouchpadPressure { .. }
                | WindowEvent::Touch(_)
        );
        let mut order: Vec<(Part, NodeId)> = self
            .host_parts
            .iter()
            .copied()
            .zip(child_nodes)
            .rev()
            .filter(|(part, _)| !self.part_inert(*part) && !self.part_hidden(*part))
            .collect();
        if pointer {
            let p = self.last_cursor;
            // A header row in the window's bar stands over the content: over
            // the row, the pointer is the header's, never the content's under it.
            if self.inputs.borrow().header_in_window_bar()
                && self.geometry.header_height > 0.0
                && self.geometry.window_row.contains(p)
            {
                order.retain(|(part, _)| {
                    !matches!(
                        part,
                        Part::Body(_) | Part::Rail | Part::Filter | Part::Controls
                    )
                });
            }
            // Over an open drawer, the pointer is the drawer's alone: what
            // it does not take never falls through to the content under it.
            if let Some(column) = self.geometry.open_drawer()
                && self.geometry.column(column).contains(p)
            {
                order.retain(|(part, _)| self.part_column(*part) == Some(column));
            }
            let rank = |part: &Part| -> u8 {
                if self
                    .part(*part)
                    .is_some_and(|w| w.modal_capture().is_some())
                {
                    0
                } else if self.part_area(*part).contains(p) {
                    1
                } else {
                    2
                }
            };
            order.sort_by_key(|(part, _)| rank(part));
        }
        for (part, node) in order {
            if *ctx.handled {
                break;
            }
            let path = ctx.path;
            if let Some(widget) = self.part_mut(part) {
                widget.handle_event(event, &mut ctx.reborrow(path, node));
            }
        }
    }
}

#[cfg(test)]
mod tests;
