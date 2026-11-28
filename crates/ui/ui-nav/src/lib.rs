//! `NavShell` — the one window shell: an optional sidebar of destinations, an optional overview
//! list beside the content, the content, and an optional inspector, laid out at
//! `theme.tokens.metrics`.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::collapsible_if,
    clippy::default_trait_access,
    clippy::doc_markdown,
    clippy::empty_line_after_doc_comments,
    clippy::format_collect,
    clippy::format_push_string,
    clippy::implicit_hasher,
    clippy::indexing_slicing,
    clippy::items_after_statements,
    clippy::items_after_test_module,
    clippy::large_enum_variant,
    clippy::large_stack_arrays,
    clippy::manual_assert_eq,
    clippy::manual_find,
    clippy::manual_midpoint,
    clippy::many_single_char_names,
    clippy::map_unwrap_or,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_errors_doc,
    clippy::needless_collect,
    clippy::needless_pass_by_value,
    clippy::needless_range_loop,
    clippy::needless_raw_string_hashes,
    clippy::or_fun_call,
    clippy::panic_in_result_fn,
    clippy::redundant_clone,
    clippy::redundant_closure,
    clippy::redundant_closure_for_method_calls,
    clippy::ref_as_ptr,
    clippy::result_large_err,
    clippy::semicolon_if_nothing_returned,
    clippy::significant_drop_in_scrutinee,
    clippy::significant_drop_tightening,
    clippy::similar_names,
    clippy::struct_excessive_bools,
    clippy::too_many_lines,
    clippy::type_complexity,
    clippy::unchecked_time_subtraction,
    clippy::unicode_not_nfc,
    clippy::uninlined_format_args,
    clippy::unnecessary_literal_bound,
    clippy::unnecessary_wraps,
    clippy::unreadable_literal,
    clippy::unused_self,
    clippy::used_underscore_binding,
    clippy::useless_conversion,
    reason = "lint debt these widgets carried in the toolkit, scoped to this target"
)]
#![cfg_attr(
    test,
    allow(
        clippy::disallowed_methods,
        clippy::string_slice,
        clippy::unreachable,
        clippy::float_cmp,
        reason = "in a test these are the assertion, not a hazard, and tests may wait on the real clock"
    )
)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use tilo_sort::CompactSort as _;
use tilo_ui_base::num::f64_of_usize;

use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use tilo_ui::time::Instant;
use tilo_ui::widget::flap::{FlapState, SplitLayout};
use tilo_ui::widget::window_header::header_row::HeaderRowMetrics;
use tilo_ui::widget::window_header::{HistoryControl, WindowHeader};
use tilo_ui::{
    widget::Button, widget::ButtonRole, widget::DescribedList, widget::NavList, widget::SearchField,
};
use tilo_ui_base::LocalState;
use tilo_ui_base::icons::Icon;
use tilo_ui_base::local_state::LocalBinding;
use tilo_ui_base::text_render::TextRenderer;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::BreakpointTracker;
use tilo_ui_base::{platform::HostShortcut, platform::PlatformConventions};
use tilo_ui_base::{
    widget::AppModel, widget::Element, widget::ModelPath, widget::StructureChange,
    widget::TaffyTreeExt, widget::Widget, widget::WidgetLayoutError,
};
use tilo_ui_base::{
    widget::manual::ManualContext, widget::manual::ManualInvalidation, widget::manual::ManualPolicy,
};

mod back;
mod band;
mod header_parts;
mod inspector;
mod layout;
mod memory;
/// The sidebar widths a person set, remembered for the app that runs here.
#[cfg(feature = "native")]
pub use memory::remember_for_app as remember_sidebar_widths_for_app;
mod nothing_listed;
mod notices;
mod open_states;
mod parts;
mod place;
use place::{measure, place_parts};
pub(crate) mod sidebar_resize;
mod status_action;
mod swap;
mod window_header;
mod window_toolbar;
use band::plan;
use header_parts::HeaderTitle;
use inspector::inspector_layout_of;
pub use inspector::{InspectorFrame, InspectorLink};
pub use layout::NavGeometry;
use layout::{layout_of, stack_index};
pub use notices::HeldNotice;
pub use sidebar_resize::AdaptiveSidebar;
use tilo_ui::widget::OverflowButton;
use tilo_ui_base::widget::ContentKind;
use window_toolbar::Requests;
pub use window_toolbar::{WINDOW_BACK, WINDOW_SIDEBAR, WindowBarLink};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NavMode {
    /// The first screen of a push stack: one column fills the window and choosing a destination
    /// pushes the content behind a Back button.
    #[default]
    Push,
    /// Always docked: a compact icon rail (`NavList::rail`) that fits beside
    /// the content at every width.
    Docked,
}

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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InspectorPlacement {
    #[default]
    None,
    Docked,
    /// Too narrow to dock: a drawer in from the window's trailing edge, over the content (the
    /// inspector's own counterpart of a sidebar drawer).
    Drawer,
    /// Too narrow to dock: a sheet up from the window's foot, over the
    /// content's lower part ([`tilo_ui::widget::BottomSheet`]).
    Sheet,
}

/// How a sidebar stands in a window too narrow to dock it, when the app says (rather than the
/// shell deciding from the content's kind).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SidebarCollapse {
    /// The shell's own rule: a drawer for content that is the window's point, else a screen of
    /// the push stack.
    #[default]
    Auto,
    /// A drawer over the content, opened from the bar's toggle.
    Drawer,
    /// The first screen of the push stack, with Back.
    Screen,
}

/// How an inspector stands in a window too narrow to dock it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InspectorCollapse {
    /// A sheet up from the window's foot (the shell's own rule).
    #[default]
    Sheet,
    /// A drawer in from the window's trailing edge, over the content.
    Drawer,
}

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
pub enum Column {
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

pub fn at<T>(array: &[T; 4], column: Column) -> &T {
    let [sidebar, overview, content, inspector] = array;
    match column {
        Column::Sidebar => sidebar,
        Column::Overview => overview,
        Column::Content => content,
        Column::Inspector => inspector,
    }
}

pub fn at_mut<T>(array: &mut [T; 4], column: Column) -> &mut T {
    let [sidebar, overview, content, inspector] = array;
    match column {
        Column::Sidebar => sidebar,
        Column::Overview => overview,
        Column::Content => content,
        Column::Inspector => inspector,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Part {
    Body(Column),
    /// The overview's rail, in place of the overview where the list
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
    /// The button beside the window's sync status (Retry), while it has one.
    StatusAction,
    /// The band across the window's foot ([`NavShell::bottom_band`]).
    Band,
}

pub struct Header {
    title: HeaderTitle,
    pub binding: Option<LocalBinding<String>>,
    pub actions: Vec<Element>,
    pub more: OverflowButton,
    pub leading: Option<Element>,
}

impl Header {
    fn new(column: Column) -> Self {
        Self {
            title: HeaderTitle::new(),
            binding: None,
            actions: Vec::new(),
            more: OverflowButton::new(&format!("nav-shell-more-{}", column.key())),
            leading: None,
        }
    }
}

struct Staged(Option<Element>);

impl Staged {
    fn is_some(&self) -> bool {
        self.0.is_some()
    }
}

#[derive(Default)]
struct Pending {
    bodies: [Option<Staged>; 4],
    actions: [Option<Vec<Element>>; 4],
    leading: [Option<Staged>; 4],
    controls: Option<Staged>,
    rail: Option<Staged>,
    empty: Option<Staged>,
}

impl Pending {
    fn is_empty(&self) -> bool {
        self.bodies.iter().all(Option::is_none)
            && self.actions.iter().all(Option::is_none)
            && self.leading.iter().all(Option::is_none)
            && self.controls.is_none()
            && self.rail.is_none()
            && self.empty.is_none()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Metrics {
    pub row: HeaderRowMetrics,
    pub header: f64,
    pub chip: f64,
    pub inset: f64,
    pub gap: f64,
    pub sidebar: f64,
    pub sidebar_min: f64,
    pub sidebar_max: f64,
    pub rail: f64,
    pub overview: f64,
    /// The widest the overview grows to fit what it lists.
    pub overview_max: f64,
    pub inspector: f64,
    /// The inspector's docked width before larger text widens it: a
    /// sheet in a window short of twice that opens at the whole height.
    pub sheet_least_half: f64,
    pub drawer: f64,
    pub drawer_dismiss_band: f64,
    pub list_min: f64,
    pub reading_min: f64,
    pub page_min: f64,
    pub grid_min: f64,
    pub compact_min: f64,
    pub hysteresis: f64,
    pub filter: f64,
    pub resize_grip: f64,
    pub pointer: f64,
    pub scale: f64,
    /// One line of body text: the unit of a page column's width.
    pub body_line: f64,
    /// One `rem` in logical pixels ([`tilo_ui::widget::arrange::rem_of`]): the unit the width
    /// classes are measured in, so the content beside a sidebar keeps a column's width at every
    /// text size.
    pub rem: f32,
    pub reading_measure: f64,
    pub scroll_gutter: f64,
    /// A page column's inset: a `Column`'s own padding (`s6`).
    pub page_pad: f64,
}

impl Metrics {
    /// The narrowest content of `kind` still works at.
    fn content_min(&self, kind: ContentKind) -> f64 {
        match kind {
            ContentKind::List => self.list_min,
            ContentKind::Reading | ContentKind::Document => self.reading_min,
            ContentKind::Page => self.page_min,
            ContentKind::Grid | ContentKind::Canvas => self.grid_min,
        }
    }

    /// The narrowest content of `kind` stands at beside a sidebar: its own minimum, and never
    /// below the width class it is in there ([`ContentKind::beside_sidebar_class`]).
    pub fn beside_sidebar_min(&self, kind: ContentKind) -> f64 {
        self.content_min(kind)
            .max(f64::from(tilo_ui::widget::arrange::class_min_width(
                kind.beside_sidebar_class(),
                self.rem,
            )))
    }

    pub fn resolve(theme: &Theme) -> Self {
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
            overview_max: f64::from(m.overview_max_width) * growth,
            inspector: f64::from(m.inspector_width) * growth,
            sheet_least_half: f64::from(m.inspector_width),
            drawer: f64::from(m.drawer_width) * growth,
            drawer_dismiss_band: f64::from(m.min_pointer_target + s.s4),
            list_min: f64::from(m.list_min_width) * growth,
            reading_min: f64::from(m.reading_min_width) * growth,
            page_min: f64::from(m.page_min_width) * growth,
            grid_min: f64::from(m.grid_min_width) * growth,
            compact_min: f64::from(m.window_min_width) * growth,
            hysteresis: f64::from(m.layout_hysteresis),
            filter: tilo_ui_base::widget::text_control_height(
                f64::from(tilo_ui_base::tokens::Metrics::BASE.min_pointer_target),
                theme,
            ),
            resize_grip: f64::from(s.s1),
            pointer: f64::from(m.min_pointer_target),
            scale: theme.responsive_scale(),
            body_line: f64::from(theme.text(tilo_ui_base::TextRole::Body).line_height_px()),
            rem: tilo_ui::widget::arrange::rem_of(theme),
            reading_measure: f64::from(m.reading_measure) * growth,
            scroll_gutter: f64::from(m.scrollbar_gutter),
            page_pad: f64::from(s.s6),
        }
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::resolve(&Theme::dark())
    }
}

#[derive(Clone, Debug, Default)]
struct ColumnFit {
    capable: Vec<bool>,
    compact_now: Vec<bool>,
    full: Vec<Option<f64>>,
    compact: bool,
    /// The index of the first action that belongs in the menu: the row shows its everyday actions
    /// and the menu holds the rest. `None` while they all fit.
    menu_from: Option<usize>,
    /// The index of the action the row never gives up: the one the app marks primary, the last
    /// action that is a control rather than a button, or the first that stands in the row rather
    /// than in its menu.
    primary: Option<usize>,
    /// The indices of the actions that are controls the app hosts in the row (a strip of
    /// choices, a stepper) rather than buttons: the row fits each to the room it has left
    /// instead of naming it in a menu that cannot run it.
    controls: Vec<usize>,
    /// How many actions stand in the row; the rest are in the More menu.
    /// `None` until the first layout decides.
    shown: Option<usize>,
    /// The one control the row stands alone, every button waiting in its menu: `None` unless
    /// the column had no room for a button beside it.
    control_only: Option<usize>,
    /// The app's own primary action, which keeps its words longest.
    words: Option<usize>,
    /// Whether that action keeps its words while the row's secondary actions show their icons.
    words_labelled: bool,
    header: Option<HeaderRowFacts>,
}

/// What one header row laid itself out as: the width it gave its title against what the title
/// says, what it left beside it, and what its actions kept. The shell names the column and says
/// whether the title reads, which only the title's own words can answer.
#[derive(Clone, Copy, Debug)]
struct HeaderRowFacts {
    title_needs: f64,
    title_has: f64,
    free_beside_title: f64,
    labelled_action: bool,
    primary_shown: bool,
}

/// Everything the geometry depends on. Shared with the layout policy the
/// host runs, so the policy and the shell compute the same rects.
#[allow(
    clippy::struct_excessive_bools,
    reason = "which parts the app filled are independent facts"
)]
#[derive(Clone, Debug)]
struct Inputs {
    band: band::Band,
    mode: NavMode,
    kind: ContentKind,
    /// The content is one centred page column this many lines of body text
    /// wide ([`NavShell::with_page_column`]).
    page_lines: Option<f64>,
    metrics: Metrics,
    has_sidebar: bool,
    /// The window floats over the shell as a card ([`tilo_ui::window_toolbar::floats`]): too narrow
    /// to dock its sidebar, it shows its content alone.
    floats: bool,
    /// The app shows one thing alone ([`crate::WindowBarLink::show_content_alone`]): no
    /// sidebar is placed at any width.
    alone: bool,
    has_overview: bool,
    /// The window shows a content column. A shell with nothing to show beside its overview gives
    /// the window to the overview until something is shown ([`NavShell::content_shown`]).
    has_content: bool,
    has_rail: bool,
    has_inspector: bool,
    has_filter: bool,
    has_controls: bool,
    history: Option<HistoryControl>,
    status_action: bool,
    /// Area for the window's controls at the trailing end of the row, while
    /// the host merged the window's bar into it (a maximized window).
    window_controls: Option<f64>,
    /// The window's bar shows the sidebar toggle among the app's parts
    /// ([`NavShell::set_sidebar_toggle_in_window_bar`]).
    toggle_in_bar: bool,
    /// The app's toolbar is its window bar's parts
    /// ([`NavShell::with_window_toolbar`]): the shell draws no header row.
    toolbar_in_bar: bool,
    /// The host lists the window's actions where the person reaches them; actions that do not fit
    /// give way with no other API.
    actions_listed: bool,
    titled: [bool; 4],
    actions: [usize; 4],
    leading: [bool; 4],
    /// The app's own page stack can go back ([`NavShell::with_back`]).
    content_back: bool,
    /// What the content would list is empty, so a whole-window empty state stands in its column
    /// ([`NavShell::nothing_listed`]).
    nothing_listed: bool,
    /// The docked sidebar width a person dragged to, authored at 1×.
    user_sidebar: Option<f64>,
    adaptive: Option<AdaptiveSidebar>,
    /// How the sidebar stands when the window is too narrow to dock it.
    sidebar_collapse: SidebarCollapse,
    /// How the inspector stands when the window is too narrow to dock it.
    inspector_collapse: InspectorCollapse,
    sidebar: FlapState,
    inspector: FlapState,
    sheet: tilo_ui::widget::BottomSheet,
    pane: NavPane,
    /// The drawn position in the push stack while a push slides; `None` at
    /// rest, where the front screen's index stands.
    push_pos: Option<f64>,
    /// The push stack's position on the drill motion (`motion::Drill`).
    push: tilo_ui_base::motion::MotionValue<f64>,
    /// The parts in the order they were built (their placement index).
    parts: Vec<Part>,
    fit: Rc<RefCell<[ColumnFit; 4]>>,
    held: Held,
}

/// Which side of each threshold the last applied layout stood on (`None` before the first).
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
            rail_fits: Some(g.side_by_side),
        }
    }
}

/// Whether `spare` reaches `threshold`, given the side the layout stood on
/// last: a layout that does not hold needs `band` more width to come back.
fn reaches(spare: f64, threshold: f64, held: Option<bool>, band: f64) -> bool {
    let threshold = if held == Some(false) {
        threshold + band
    } else {
        threshold
    };
    spare >= threshold - 1e-6
}

impl Default for Inputs {
    fn default() -> Self {
        Self {
            band: band::Band::default(),
            status_action: false,
            mode: NavMode::Push,
            kind: ContentKind::default(),
            page_lines: None,
            metrics: Metrics::default(),
            has_sidebar: false,
            floats: false,
            alone: false,
            has_overview: false,
            has_content: true,
            has_rail: false,
            has_inspector: false,
            has_filter: false,
            has_controls: false,
            history: None,
            window_controls: None,
            toggle_in_bar: false,
            actions_listed: false,
            titled: [false; 4],
            actions: [0; 4],
            leading: [false; 4],
            content_back: false,
            nothing_listed: false,
            toolbar_in_bar: false,
            user_sidebar: None,
            adaptive: None,
            sidebar_collapse: SidebarCollapse::Auto,
            inspector_collapse: InspectorCollapse::Sheet,
            sidebar: FlapState::open_when_docked(),
            inspector: FlapState::open_when_docked(),
            sheet: tilo_ui::widget::BottomSheet::new(),
            pane: NavPane::Content,
            push_pos: None,
            push: tilo_ui_base::motion::MotionValue::new(0.0),
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
        let (default, narrowest) = (self.sidebar_default(), self.sidebar_narrowest());
        self.user_sidebar
            .map_or(default, |authored| authored * m.scale)
            .clamp(narrowest, m.sidebar_max.max(narrowest))
    }

    fn content_min(&self) -> f64 {
        if !self.has_content {
            return 0.0;
        }
        self.metrics.content_min(self.kind)
    }

    /// The narrowest the content stands at beside a sidebar: a column of the window's own where
    /// the content is a column of text, never a phone's screen ([`Metrics::beside_sidebar_min`]).
    fn content_beside_sidebar_min(&self) -> f64 {
        if !self.has_content {
            return 0.0;
        }
        self.metrics.beside_sidebar_min(self.kind)
    }

    /// The width class the content beside a sidebar is in at the least
    /// ([`ContentKind::beside_sidebar_class`]).
    fn content_beside_sidebar_class(&self) -> tilo_ui_base::widget::WidthClass {
        if !self.has_content {
            return tilo_ui_base::widget::WidthClass::Compact;
        }
        self.kind.beside_sidebar_class()
    }

    /// The narrowest window the compact layout works at: one column at a time (or the content under
    /// its drawer), never narrower than phone width; a rail stays beside the content.
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
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "a reveal is clamped to 0..=1, always well within f32 range"
)]
fn unit_f32(v: f64) -> f32 {
    v.clamp(0.0, 1.0) as f32
}

#[derive(Clone, Copy, Debug)]
struct Resize {
    start_x: f64,
    start_width: f64,
}

struct ContentBack {
    can_go_back: Box<dyn Fn() -> bool>,
    go_back: Box<dyn FnMut()>,
}

/// An open/closed state the app keeps in a [`LocalState`].
struct OpenBinding {
    binding: LocalBinding<bool>,
    wanted: Option<bool>,
}

/// What the host walks in place of a push screen that is not in front: no focus, no semantics, no
/// children, though the shell still lays it out, paints and syncs it.
#[derive(Debug, Default)]
struct InertPart;

static INERT_PART: InertPart = InertPart;

/// The action target of a command behind a column's "…": the column and the action's index in
/// its header, while the menu is shut.
const MORE_TARGET: &str = "nav-more:";

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

#[allow(
    clippy::struct_excessive_bools,
    reason = "structure, layout and paint dirtiness and the cross-fade are independent flags"
)]
pub struct NavShell {
    id: Option<String>,
    sidebar: Option<Element>,
    overview: Option<Element>,
    content: Element,
    /// Whether the window shows a content column: a window whose content only stands in for what
    /// the overview has not opened yet gives its width to the overview
    /// ([`Self::content_shown`]).
    content_shown: bool,
    /// [`Self::content_shown`] as the shell was last built: the column comes or goes when the two
    /// differ as the change is taken, so a body swapped for another (the content hidden and shown
    /// again in one change) keeps the column where it stands.
    content_shown_built: bool,
    inspector: Option<Element>,
    /// The app's columns while [`Self::nothing_listed`] stands in for them.
    held: Option<nothing_listed::Held>,
    rail: Option<Element>,
    filter: Option<Box<SearchField>>,
    controls: Option<Element>,
    headers: Box<[Header; 4]>,
    pending: Pending,
    toggle: Box<Button>,
    back: Box<Button>,
    inspector_toggle: Box<Button>,
    history_open: Box<Button>,
    history_close: Box<Button>,
    requests: Rc<Requests>,
    pane_binding: Option<LocalBinding<NavPane>>,
    sidebar_state: Option<OpenBinding>,
    inspector_state: Option<OpenBinding>,
    /// The open state a view last asked for, per column: applied when the view asks for another,
    /// never reasserted over the person's own toggle.
    sidebar_open_asked: Option<bool>,
    inspector_open_asked: Option<bool>,
    content_back: Option<ContentBack>,
    /// The content pane the person last worked in; a collapse to narrow
    /// keeps it in front.
    active_hint: Option<NavPane>,
    /// The list the person last chose a destination in (the sidebar, or the overview and its
    /// rail), while they have done nothing since: the focus it still holds is the row they
    /// pressed, so a collapse shows where that row led rather than the list again.
    chose_in: Option<NavPane>,

    inputs: Rc<RefCell<Inputs>>,
    policy: Rc<tilo_ui_base::widget::manual::Policy>,
    invalidation: ManualInvalidation,
    baseline: Vec<(NodeId, Style)>,
    /// The parts in the host tree, in build order.
    laid_out_parts: Vec<Part>,
    inert_parts: Vec<InertPart>,
    layout_theme: Box<Theme>,
    conventions: PlatformConventions,

    rect: Rect,
    geometry: NavGeometry,
    /// The size class of the rect the last layout was solved in
    /// ([`Breakpoint`], held across passes through the hysteresis band).
    size_class: BreakpointTracker,
    last_stack: Option<Vec<NavPane>>,
    /// The push stack and front screen the tree was built for before any layout said how wide the
    /// shell is: the first layout rebuilds only when it lands elsewhere.
    planned: Option<(Vec<NavPane>, NavPane)>,
    pane_chosen: bool,
    resize: Option<Resize>,
    /// The sidebar's resize edge holds keyboard focus (given through its
    /// accessible separator); arrows then resize.
    resize_focused: bool,
    edge: sidebar_resize::Edge,
    last_cursor: Point,
    fade: tilo_ui_base::motion::Presence,
    /// The columns a layout switch brought in fading in: those that appeared or moved where no
    /// slide carries them, and under reduced motion every one of them.
    switch_fade: tilo_ui_base::motion::Presence,
    /// Which columns ([`Column::index`]) the layout switch fades in.
    switch_columns: [bool; 4],
    /// What the content shows, as its owner names it (`NavShellProps::content_key`): when it
    /// changes, the new content is faded in where the old one stood.
    content_key: Option<String>,
    /// The fade of the content a change of [`Self::content_key`] brings.
    content_fade: tilo_ui_base::motion::Presence,
    /// A slide, push or fade is moving, or its last pose has yet to be laid
    /// out: the host owes the shell a frame.
    animating: bool,
    /// The frame after a motion ended rebuilds the structure (a part the
    /// last pose hides goes): the host owes the shell that frame too.
    settling: bool,
    /// A slide, push or switch of the columns moved in the motion now running, so the part its
    /// last pose hides goes when it ends. The content's own cross-fade moves no part and leaves
    /// the structure standing.
    slid: bool,
    built: bool,
    /// The window's bar presents search for the window, so the headers carry no button of their own
    /// that opens the app's search.
    window_search: bool,
    /// Header actions put away while the host may still hold their nodes,
    /// dropped when the next build replaces the row.
    retired_actions: Vec<Element>,
    structure_dirty: bool,
    /// Bodies are staged for columns that stand laid out, and nothing else: the next remount puts
    /// them in place under the shell's node ([`Self::swap_bodies`]), and the rest of the window
    /// keeps its layout.
    bodies_swapped: bool,
    layout_dirty: bool,
    paint_dirty: bool,
    /// The window's notice, shown in its bar ([`Self::toast`]).
    notice: tilo_ui::widget::toast::Transient<notices::ShellNotice>,
    /// The notice whose button the person pressed, for
    /// [`Self::take_toast_action`].
    notice_pressed: Option<String>,
    sync_status: tilo_ui::widget::HeaderStatus,
    /// The status changed since the last frame drawn: only where it was
    /// ([`Self::status_drawn`]) and where it is repaint, not the whole shell.
    status_dirty: bool,
    status_drawn: Rect,
    /// The span of the header row the status was last painted into; empty
    /// until the shell has painted, when where the status goes is unknown.
    status_span: Rect,
    status_action: Option<Button>,
    band: Option<Element>,
    status_action_spec: Option<tilo_ui::widget::StatusAction>,
    retired_status_actions: Vec<Button>,
    /// Whether Back goes back now, for a shell made from its description with a `back`
    /// event: taken in place as the description changes.
    can_back: Option<Rc<std::cell::Cell<bool>>>,
    /// The part a host is changing through [`tilo_ui_base::component::Described::part_views`].
    part_edit: Option<component::PartEdit>,
    /// The window's search narrows the sidebar's list wherever it stands
    /// ([`Self::search_sidebar`]).
    sidebar_search: bool,
    /// How the person reaches the window ([`Self::hear_input`]).
    interaction: tilo_ui_base::interaction::InteractionEnvironment,
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
        let policy: Rc<tilo_ui_base::widget::manual::Policy> =
            Rc::new(move |ctx: &mut ManualContext<'_>| place_parts(&policy_inputs.borrow(), ctx));
        let toggle = Self::shell_button(
            WINDOW_SIDEBAR,
            Icon::PanelLeft,
            &tilo_ui_base::i18n::I18nManager::lookup_plain("window-toolbar-hide-sidebar"),
            {
                let requests = Rc::clone(&requests);
                move || requests.toggle.set(true)
            },
        );
        let back = Self::shell_button(
            WINDOW_BACK,
            Icon::ChevronLeft,
            &tilo_ui_base::i18n::I18nManager::lookup_plain("window-toolbar-back"),
            {
                let requests = Rc::clone(&requests);
                move || requests.back.set(true)
            },
        );
        let inspector_toggle = Self::shell_button(
            "nav-shell-inspector",
            Icon::PanelRight,
            &tilo_ui_base::i18n::I18nManager::lookup_plain("window-toolbar-hide-inspector"),
            {
                let requests = Rc::clone(&requests);
                move || requests.inspector.set(true)
            },
        );
        Self {
            id: None,
            sidebar: None,
            overview: None,
            content: Box::new(tilo_ui_base::widget::Spacer::new()),
            content_shown: true,
            content_shown_built: true,
            held: None,
            inspector: None,
            rail: None,
            filter: None,
            controls: None,
            headers: Box::new(Column::ALL.map(Header::new)),
            pending: Pending::default(),
            toggle: Box::new(toggle),
            back: Box::new(back),
            inspector_toggle: Box::new(inspector_toggle),
            history_open: Box::new(tilo_ui::history_api::history_control_button(
                "nav-shell-history",
                HistoryControl::Closed,
            )),
            history_close: Box::new(tilo_ui::history_api::history_control_button(
                "nav-shell-history",
                HistoryControl::Open,
            )),
            requests,
            pane_binding: None,
            sidebar_state: None,
            inspector_state: None,
            sidebar_open_asked: None,
            inspector_open_asked: None,
            content_back: None,
            active_hint: None,
            chose_in: None,
            inputs,
            policy,
            invalidation: ManualInvalidation::default(),
            baseline: Vec::new(),
            laid_out_parts: Vec::new(),
            inert_parts: Vec::new(),
            layout_theme: Box::new(Theme::dark()),
            conventions: PlatformConventions::default(),
            rect: Rect::ZERO,
            geometry: NavGeometry::default(),
            size_class: BreakpointTracker::new(),
            last_stack: None,
            planned: None,
            pane_chosen: false,
            resize: None,
            resize_focused: false,
            edge: sidebar_resize::Edge::default(),
            last_cursor: Point::ZERO,
            fade: tilo_ui_base::motion::Presence::opened(),
            switch_fade: tilo_ui_base::motion::Presence::opened(),
            switch_columns: [false; 4],
            content_key: None,
            content_fade: tilo_ui_base::motion::Presence::opened(),
            animating: false,
            settling: false,
            slid: false,
            built: false,
            window_search: false,
            retired_actions: Vec::new(),
            structure_dirty: false,
            bodies_swapped: false,
            layout_dirty: true,
            paint_dirty: true,
            notice: tilo_ui::widget::toast::Transient::new(),
            notice_pressed: None,
            sync_status: tilo_ui::widget::HeaderStatus::default(),
            status_dirty: false,
            status_drawn: Rect::ZERO,
            status_span: Rect::ZERO,
            status_action: None,
            band: None,
            status_action_spec: None,
            retired_status_actions: Vec::new(),
            can_back: None,
            part_edit: None,
            sidebar_search: false,
            interaction: tilo_ui_base::interaction::InteractionEnvironment::default(),
        }
    }

    fn shell_button(id: &str, icon: Icon, name: &str, on_press: impl FnMut() + 'static) -> Button {
        Button::icon_only(id, icon, name).on_open(on_press)
    }

    /// The sidebar of destinations (usually a [`tilo_ui::widget::NavList`]).
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

    /// What the overview collapses into where the list no longer fits beside the content but a rail
    /// does: a `NavList::rail` of the same places, so the window narrows to rail and detail before
    /// it pushes.
    #[must_use]
    pub fn overview_rail(mut self, rail: Element) -> Self {
        self.set_overview_rail(Some(rail));
        self
    }

    #[must_use]
    pub fn content(mut self, content: Element) -> Self {
        self.set_content(content);
        self
    }

    /// Whether the window shows a content column.
    #[must_use]
    pub fn content_shown(mut self, shown: bool) -> Self {
        self.set_content_shown(shown);
        self
    }

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

    #[must_use]
    pub fn overview_header(mut self, title: impl Into<String>, actions: Vec<Element>) -> Self {
        self.set_overview_header(title, actions);
        self
    }

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

    /// The fill of the content's header between the shell's leading buttons and the actions: a path
    /// bar, or a mode or tab switcher with no title; it stands at its own width and takes more width
    /// when it needs it.
    #[must_use]
    pub fn content_header_leading(mut self, leading: Element) -> Self {
        self.set_content_header_leading(Some(leading));
        self
    }

    #[must_use]
    pub fn inspector_header(mut self, title: impl Into<String>, actions: Vec<Element>) -> Self {
        self.set_inspector_header(title, actions);
        self
    }

    #[must_use]
    pub fn overview_count(mut self, count: usize) -> Self {
        self.set_count(Column::Overview, Some(count));
        self
    }

    #[must_use]
    pub fn content_count(mut self, count: usize) -> Self {
        self.set_count(Column::Content, Some(count));
        self
    }

    /// The one filter field of the window, scoped to the sidebar's list (or the overview, else the
    /// content); Ctrl+F focuses it, and the shell carries its query to the column's `Filterable`
    /// after every event.
    #[must_use]
    pub fn with_filter(mut self, filter: SearchField) -> Self {
        self.filter = Some(Box::new(filter));
        self.parts_changed();
        self
    }

    /// The content as one page column `lines` lines of body text wide, centred in a scroller that
    /// keeps its bar's gutter; the content's filter field stands on that column's content edges so
    /// it lines up with the page under it.
    #[must_use]
    pub fn with_page_column(self, lines: f32) -> Self {
        self.inputs.borrow_mut().page_lines = Some(f64::from(lines));
        self
    }

    /// What the sidebar becomes in a narrow window: the first push screen
    /// (the default), or an icon rail docked at every width.
    #[must_use]
    pub fn mode(mut self, mode: NavMode) -> Self {
        self.inputs.borrow_mut().mode = mode;
        self.parts_changed();
        self
    }

    /// What the content is (`ContentKind`): the shell docks the sidebar, inspector and overview
    /// while the content keeps the minimum its kind needs, and the window's narrowest width is that
    /// minimum's compact layout.
    #[must_use]
    pub fn content_kind(mut self, kind: ContentKind) -> Self {
        self.inputs.borrow_mut().kind = kind;
        self.parts_changed();
        self
    }

    /// How the sidebar stands in a window too narrow to dock it: the shell's own rule
    /// ([`SidebarCollapse::Auto`]), a drawer over the content, or a screen of the push stack.
    #[must_use]
    pub fn sidebar_collapse(mut self, collapse: SidebarCollapse) -> Self {
        self.inputs.borrow_mut().sidebar_collapse = collapse;
        self.parts_changed();
        self
    }

    /// How the inspector stands in a window too narrow to dock it: a sheet up from the window's
    /// foot ([`InspectorCollapse::Sheet`]) or a drawer in from its trailing edge
    /// ([`InspectorCollapse::Drawer`]).
    #[must_use]
    pub fn inspector_collapse(mut self, collapse: InspectorCollapse) -> Self {
        self.inputs.borrow_mut().inspector_collapse = collapse;
        self.parts_changed();
        self
    }

    /// Show or hide the sidebar, as a view asks: the docked column and the overlay drawer open
    /// or close together, at the shell's motion (reduced motion lands at once).
    pub fn set_sidebar_open(&mut self, open: bool) {
        if !self.has_sidebar() {
            return;
        }
        self.inputs
            .borrow_mut()
            .sidebar
            .from_input(tilo_ui_base::tokens::MotionSource::Action);
        self.inputs.borrow_mut().sidebar.set_open(open);
        self.after_flap_change();
    }

    /// Show or hide the inspector, as a view asks: the docked column, the sheet and the drawer
    /// open or close together, at the shell's motion (reduced motion lands at once).
    pub fn set_inspector_open(&mut self, open: bool) {
        if !self.has_inspector() {
            return;
        }
        self.inputs
            .borrow_mut()
            .inspector
            .from_input(tilo_ui_base::tokens::MotionSource::Action);
        self.inputs.borrow_mut().inspector.set_open(open);
        self.after_flap_change();
    }

    /// The open state a view asks for the sidebar (`sidebar_open`): applied when the view asks
    /// for a different one, never reasserted over the person's own toggle.
    pub fn show_sidebar_from_view(&mut self, open: Option<bool>) {
        if self.sidebar_open_asked == open {
            return;
        }
        self.sidebar_open_asked = open;
        if let Some(open) = open {
            self.set_sidebar_open(open);
        }
    }

    /// The open state a view asks for the inspector (`inspector_open`): applied when the view
    /// asks for a different one, never reasserted over the person's own toggle.
    pub fn show_inspector_from_view(&mut self, open: Option<bool>) {
        if self.inspector_open_asked == open {
            return;
        }
        self.inspector_open_asked = open;
        if let Some(open) = open {
            self.set_inspector_open(open);
        }
    }

    /// How wide the pane beside the sidebar stands: the window without the columns that dock
    /// beside it (the sidebar's own and the inspector's).
    fn beside_sidebar(&self) -> f64 {
        let g = &self.geometry;
        let docked = |placement: bool, width: f64| if placement { width } else { 0.0 };
        (g.size.x
            - docked(
                g.sidebar == SidebarPlacement::Docked,
                g.sidebar_column.width(),
            )
            - docked(
                g.inspector == InspectorPlacement::Docked,
                g.inspector_column.width(),
            ))
        .max(0.0)
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

    /// The window's search narrows the sidebar's list ([`tilo_ui::widget::Filterable`]) wherever
    /// it stands: also while a narrow window shows the content screen, whose content may then
    /// show what the list's filter finds.
    #[must_use]
    pub fn search_sidebar(mut self) -> Self {
        self.sidebar_search = true;
        self
    }

    #[must_use]
    pub fn with_sidebar_hidden(self) -> Self {
        self.inputs.borrow_mut().sidebar.snap(false);
        self
    }

    #[must_use]
    pub fn with_inspector_hidden(self) -> Self {
        self.inputs.borrow_mut().inspector.snap(false);
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

    /// Bind the content's Back to the app's page stack: while `stack` can go back the shell
    /// shows its Back chip at the leading edge of the content's header, and pressing it pops
    /// the stack (browse history, a drill-down from a dashboard into a queue).
    #[must_use]
    pub fn with_back<T>(self, stack: &LocalState<tilo_ui_base::NavStack<T>>) -> Self
    where
        T: Clone + PartialEq + 'static,
    {
        let read = stack.clone();
        let write = stack.clone();
        self.on_back(
            move || read.with(tilo_ui_base::NavStack::can_go_back),
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
        self.parts_changed();
        self
    }

    pub fn set_sidebar(&mut self, sidebar: Option<Element>) {
        self.stage_body(Column::Sidebar, sidebar);
    }

    pub fn set_overview(&mut self, overview: Option<Element>) {
        self.stage_body(Column::Overview, overview);
    }

    pub fn set_content(&mut self, content: Element) {
        self.content_shown = true;
        self.stage_body(Column::Content, Some(content));
    }

    pub fn set_content_shown(&mut self, shown: bool) {
        if self.content_shown == shown {
            return;
        }
        self.content_shown = shown;
        self.refresh_part_inputs();
        self.invalidate();
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
        if let Some(part) = self.pending.leading.get_mut(Column::Content.index()) {
            *part = Some(Staged(leading));
        }
        self.pending_changed();
    }

    fn stage_body(&mut self, column: Column, element: Option<Element>) {
        if let Some(part) = self.pending.bodies.get_mut(column.index()) {
            *part = Some(Staged(element));
        }
        if self.built && self.swaps_in_place() {
            self.bodies_swapped = true;
            self.refresh_part_inputs();
            self.invalidate();
        } else {
            self.pending_changed();
        }
    }

    pub fn set_sidebar_header(&mut self, title: impl Into<String>, actions: Vec<Element>) {
        self.stage_header(Column::Sidebar, title.into(), actions);
    }

    pub fn set_overview_header(&mut self, title: impl Into<String>, actions: Vec<Element>) {
        self.stage_header(Column::Overview, title.into(), actions);
    }

    pub fn set_content_header(&mut self, title: impl Into<String>, actions: Vec<Element>) {
        self.stage_header(Column::Content, title.into(), actions);
    }

    pub fn set_inspector_header(&mut self, title: impl Into<String>, actions: Vec<Element>) {
        self.stage_header(Column::Inspector, title.into(), actions);
    }

    fn stage_header(&mut self, column: Column, title: String, mut actions: Vec<Element>) {
        if self.window_search {
            actions.retain(|action| !is_app_search(action.as_ref()));
        }
        self.set_title(column, title);
        if let Some(part) = self.pending.actions.get_mut(column.index()) {
            *part = Some(actions);
        }
        self.pending_changed();
    }

    pub fn set_sidebar_title(&mut self, title: impl Into<String>) {
        self.set_title(Column::Sidebar, title.into());
    }

    pub fn set_overview_title(&mut self, title: impl Into<String>) {
        self.set_title(Column::Overview, title.into());
    }

    pub fn set_content_title(&mut self, title: impl Into<String>) {
        self.set_title(Column::Content, title.into());
    }

    pub fn set_inspector_title(&mut self, title: impl Into<String>) {
        self.set_title(Column::Inspector, title.into());
    }

    pub fn set_content_count(&mut self, count: Option<usize>) {
        self.set_count(Column::Content, count);
    }

    pub fn set_overview_count(&mut self, count: Option<usize>) {
        self.set_count(Column::Overview, count);
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

    /// A title that comes or goes changes the header row; a shown title that says something
    /// else is the same part, measured and painted again (an item's title as it is typed).
    fn set_title(&mut self, column: Column, title: String) {
        let title = (!title.trim().is_empty()).then_some(title);
        let Some(header) = self.headers.get_mut(column.index()) else {
            return;
        };
        if header.title.title == title {
            return;
        }
        let comes_or_goes = header.title.is_shown() != title.is_some();
        header.title.title = title;
        if comes_or_goes {
            self.parts_changed();
        } else {
            self.invalidate();
        }
    }

    fn set_count(&mut self, column: Column, count: Option<usize>) {
        if let Some(header) = self.headers.get_mut(column.index())
            && header.title.count != count
        {
            header.title.count = count;
            self.invalidate();
        }
    }

    /// A part or header waits for the next build; before the first one there is no tree to wait
    /// for.
    fn drop_app_search(&mut self) {
        let mut changed = false;
        for column in Column::ALL {
            let index = column.index();
            if let Some(Some(staged)) = self.pending.actions.get_mut(index) {
                let before = staged.len();
                staged.retain(|action| !is_app_search(action.as_ref()));
                changed |= staged.len() != before;
                continue;
            }
            let Some(header) = self.headers.get_mut(index) else {
                continue;
            };
            if !header
                .actions
                .iter()
                .any(|action| is_app_search(action.as_ref()))
            {
                continue;
            }
            let (keep, gone): (Vec<Element>, Vec<Element>) = std::mem::take(&mut header.actions)
                .into_iter()
                .partition(|action| !is_app_search(action.as_ref()));
            if self.built {
                // The host may still hold the row's nodes: the row is staged
                // anew, and what it drops lives until the build replaces it.
                self.retired_actions.extend(gone);
                if let Some(part) = self.pending.actions.get_mut(index) {
                    *part = Some(keep);
                }
            } else {
                header.actions = keep;
            }
            changed = true;
        }
        if changed {
            self.pending_changed();
        }
    }

    fn pending_changed(&mut self) {
        if !self.built {
            self.apply_pending();
        }
        self.parts_changed();
    }

    /// Put the waiting parts and headers in place, dropping what they
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
            empty,
        } = pending;
        let (leave, enter) = match empty {
            Some(Staged(None)) => (Some(Staged(None)), None),
            enter => (None, enter),
        };
        self.apply_nothing_listed(leave);
        for (column, body) in Column::ALL.into_iter().zip(bodies) {
            let Some(Staged(body)) = body else { continue };
            match (self.body_part(column), body) {
                (nothing_listed::BodyPart::Optional(part), body) => *part = body,
                (nothing_listed::BodyPart::Required(part), Some(body)) => *part = body,
                (nothing_listed::BodyPart::Required(_), None) => {}
            }
        }
        self.apply_nothing_listed(enter);
        for (header, actions) in self.headers.iter_mut().zip(actions) {
            if let Some(actions) = actions {
                header.more.close();
                header.actions = actions;
            }
        }
        self.retired_actions.clear();
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

    fn reset_fit(&mut self) {
        let inputs = self.inputs.borrow();
        let mut fit = inputs.fit.borrow_mut();
        for (part, header) in fit.iter_mut().zip(self.headers.iter_mut()) {
            let fitted = fit_of(header);
            let count = fitted.capable.len();
            if part.full.len() != count {
                *part = ColumnFit {
                    full: vec![None; count],
                    ..ColumnFit::default()
                };
            }
            keep_fitted(part, fitted);
        }
    }

    fn parts_changed(&mut self) {
        self.refresh_part_inputs();
        self.structure_dirty = true;
        self.invalidate();
    }

    /// Plan the parts from the parts, headers and columns as they are now (which columns and
    /// titles there are, how many actions each holds), marking nothing.
    fn refresh_part_inputs(&self) {
        {
            let mut inputs = self.inputs.borrow_mut();
            let has = |current: bool, pending: Option<&Staged>| match pending {
                Some(next) => next.is_some(),
                None => current,
            };
            let body = |column| self.has_body(column, at(&self.pending.bodies, column).as_ref());
            inputs.has_sidebar = body(Column::Sidebar);
            inputs.has_overview = body(Column::Overview);
            // A window with nothing to show beside its overview has no content column, unless
            // what it shows is one whole-window empty state there.
            inputs.has_content = self.content_shown || self.nothing_listed_shown();
            inputs.nothing_listed = self.nothing_listed_shown();
            inputs.has_inspector = body(Column::Inspector);
            let listed = !self.nothing_listed_shown();
            inputs.has_rail = listed && has(self.rail.is_some(), self.pending.rail.as_ref());
            inputs.has_controls =
                listed && has(self.controls.is_some(), self.pending.controls.as_ref());
            inputs.has_filter =
                self.filter.is_some() && !(inputs.toolbar_in_bar && self.window_search);
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
    }

    fn invalidate(&mut self) {
        self.invalidation.invalidate();
        self.layout_dirty = true;
        self.paint_dirty = true;
    }

    /// The sidebar as the widget the app gave it (the one waiting to be
    /// shown, if a swap is pending), through its `Widget::as_any_mut`
    /// (which the widget answers with `Some(self)`).
    pub fn sidebar_mut<T: Widget + 'static>(&mut self) -> Option<&mut T> {
        let part = match at_mut(&mut self.pending.bodies, Column::Sidebar) {
            Some(Staged(pending)) => pending.as_deref_mut(),
            None => self.sidebar.as_deref_mut(),
        };
        downcast(part)
    }

    /// The sidebar as the widget the app gave it, to read: the one waiting
    /// to be shown, if a swap is pending.
    pub fn sidebar_ref<T: Widget + 'static>(&self) -> Option<&T> {
        let [sidebar, ..] = &self.pending.bodies;
        let part = match sidebar {
            Some(Staged(pending)) => pending.as_deref(),
            None => self.sidebar.as_deref(),
        };
        part.and_then(|widget| widget.as_any())
            .and_then(|any| any.downcast_ref::<T>())
    }

    pub fn overview_mut<T: Widget + 'static>(&mut self) -> Option<&mut T> {
        downcast(self.app_body_mut(Column::Overview))
    }

    /// The overview's first list, however the app wraps it inside its view of
    /// the column: the rows it shows and the person's choice among them.
    pub fn overview_list_mut(&mut self) -> Option<&mut DescribedList> {
        tilo_ui_base::widget::find_mut::<DescribedList>(self.app_body_mut(Column::Overview)?)
    }

    pub fn content_mut<T: Widget + 'static>(&mut self) -> Option<&mut T> {
        downcast(self.app_body_mut(Column::Content))
    }

    /// The window's filter field ([`Self::with_filter`]): its query and the
    /// count the filtered column reports.
    pub fn filter(&self) -> Option<&SearchField> {
        self.filter.as_deref()
    }

    pub fn filter_mut(&mut self) -> Option<&mut SearchField> {
        self.filter.as_deref_mut()
    }

    pub fn geometry(&self) -> &NavGeometry {
        &self.geometry
    }

    pub fn has_sidebar(&self) -> bool {
        self.inputs.borrow().has_sidebar
    }

    pub fn has_inspector(&self) -> bool {
        self.inputs.borrow().has_inspector
    }

    pub fn has_overview(&self) -> bool {
        self.inputs.borrow().has_overview
    }

    /// Whether the window has a sidebar but shows its content alone: a card floating over the
    /// shell, too narrow to dock it. It offers no toggle for it.
    fn sidebar_folded(&self) -> bool {
        self.built && self.has_sidebar() && self.geometry.sidebar == SidebarPlacement::None
    }

    /// Whether the window shows a content column ([`Self::content_shown`]).
    pub fn has_content(&self) -> bool {
        self.content_shown || self.nothing_listed_shown()
    }

    /// The screen a window shows for what it is about: its content, the list when nothing
    /// stands beside it, or the sidebar when it lists nothing at all.
    fn content_pane(&self) -> NavPane {
        if self.has_content() {
            NavPane::Content
        } else if self.has_overview() {
            NavPane::Overview
        } else {
            NavPane::Sidebar
        }
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

    pub fn front_pane(&self) -> NavPane {
        self.inputs.borrow().pane
    }

    /// Show or hide the sidebar, remembering the choice for this window size class.
    pub fn toggle_sidebar(&mut self) {
        if !self.has_sidebar() || self.sidebar_folded() {
            return;
        }
        if self.geometry.sidebar == SidebarPlacement::Screen {
            let pane = if self.front_pane() == NavPane::Sidebar {
                self.screen_behind_sidebar()
            } else {
                NavPane::Sidebar
            };
            self.show(pane);
            return;
        }
        let layout = layout_of(self.geometry.sidebar);
        self.inputs.borrow_mut().sidebar.toggle(layout);
        self.after_flap_change();
    }

    /// The screen a hidden sidebar screen leaves in front: the one the
    /// person last worked in, else the one the sidebar leads to.
    fn screen_behind_sidebar(&self) -> NavPane {
        let stack = &self.geometry.stack;
        self.active_hint
            .filter(|pane| *pane != NavPane::Sidebar && stack.contains(pane))
            .or_else(|| stack.iter().copied().find(|pane| *pane != NavPane::Sidebar))
            .unwrap_or(NavPane::Content)
    }

    /// A toggle a key asked for: it lands at once (motion follows the
    /// input); the next pointer toggle animates again.
    fn toggle_by_key(&mut self, column: Column) {
        {
            let mut inputs = self.inputs.borrow_mut();
            let flap = if column == Column::Inspector {
                &mut inputs.inspector
            } else {
                &mut inputs.sidebar
            };
            flap.from_input(tilo_ui_base::tokens::MotionSource::Keyboard);
        }
        if column == Column::Inspector {
            self.toggle_inspector();
        } else {
            self.toggle_sidebar();
        }
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

    /// The count the window's sidebar shows beside the place chosen in it, while that sidebar
    /// stands beside the content: the drawer and the push screen hide the columns they cover,
    /// and a compact rail draws a mark rather than the number.
    fn sidebar_place_count(&self) -> Option<u32> {
        if !self.has_sidebar() || self.geometry.sidebar != SidebarPlacement::Docked {
            return None;
        }
        if !self.inputs.borrow().sidebar.is_open(SplitLayout::Docked) {
            return None;
        }
        let list = tilo_ui_base::widget::find::<NavList>(self.sidebar.as_deref()?)?;
        if list.is_rail() {
            return None;
        }
        list.selected_count()
            .map(tilo_ui::widget::nav_list::nav_row::NavCount::value)
    }

    /// A column's header draws its count only where no visible sidebar already draws it for the
    /// place shown: the same number twice on one screen says nothing more than once.
    fn mirror_header_counts(&mut self) -> bool {
        let place = self.sidebar_place_count();
        let mut changed = false;
        for header in self.headers.iter_mut() {
            let repeated = match (header.title.count, place) {
                (Some(count), Some(place)) => u32::try_from(count).ok() == Some(place),
                _ => false,
            };
            if header.title.repeated_by_sidebar != repeated {
                header.title.repeated_by_sidebar = repeated;
                changed = true;
            }
        }
        if changed {
            self.invalidate();
        }
        changed
    }

    /// Give each column's title back to the content while the content shows the same words as
    /// its page's own heading: one title on screen, and the one the page sets in the body.
    fn mirror_own_page_titles(&mut self) -> bool {
        let mut covered = [false; 4];
        let mut edited: [Option<TitleShown>; 4] = [None, None, None, None];
        for column in Column::ALL {
            let Some(body) = self.part(Part::Body(column)) else {
                continue;
            };
            let Some(shown) = page_title_shown(body, self.geometry.body(column)) else {
                continue;
            };
            let repeats = self.headers[column.index()]
                .title
                .title
                .as_deref()
                .is_some_and(|bar| same_words(bar, &shown.words));
            if shown.heading && !repeats {
                continue;
            }
            covered[column.index()] = true;
            if !shown.heading {
                edited[column.index()] = Some(shown);
            }
        }
        let mut changed = false;
        for column in Column::ALL {
            let title = &mut self.headers[column.index()].title;
            let covered = covered[column.index()];
            let edited = edited[column.index()].clone();
            if title.is_covered() != covered {
                title.set_covered(covered);
                changed = true;
            }
            if title.edited != edited {
                title.set_edited(edited);
                changed = true;
            }
        }
        changed
    }

    fn after_flap_change(&mut self) {
        if self.layout_theme.reduce_motion {
            self.fade = Self::fading_in(self.layout_theme.motion());
        }
        self.animating = true;
        self.structure_dirty = true;
        self.update_toggle_names();
        self.publish_open_states();
        self.invalidate();
    }

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
        self.publish_window_parts();
        self.publish_inspector_frame();
    }

    /// Bring the sidebar and inspector in line with bound states that
    /// changed, once the layout knows where they stand.
    fn apply_open_states(&mut self) {
        for sidebar in [true, false] {
            let part = if sidebar {
                &mut self.sidebar_state
            } else {
                &mut self.inspector_state
            };
            let Some(state) = part.as_mut() else {
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

    /// Bring `pane` to the front of a narrow window's push stack. In a wide window every pane
    /// is already showing; this records which one a later collapse keeps in front.
    pub fn show(&mut self, pane: NavPane) {
        let source = tilo_ui_base::motion::recent_input(tilo_ui::time::now())
            .unwrap_or(tilo_ui_base::tokens::MotionSource::Action);
        self.show_from(pane, source);
    }

    /// [`Self::show`] for an open whose origin the caller knows: `source` is
    /// what asked for it ([`tilo_ui_base::tokens::MotionSource::Action`] for an
    /// agent's action, a search hit or an `open` request).
    pub fn show_from(&mut self, pane: NavPane, source: tilo_ui_base::tokens::MotionSource) {
        let pane = match pane {
            NavPane::Sidebar if !self.has_sidebar() => self.content_pane(),
            NavPane::Overview if !self.has_overview() => self.content_pane(),
            NavPane::Content if !self.has_content() => self.content_pane(),
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
            let now = tilo_ui_base::motion::frame_now();
            if !inputs.push.is_moving(now) {
                let from = f64_of_usize(stack_index(stack, inputs.pane));
                inputs.push.snap(from);
            }
            inputs.pane = pane;
            let to = f64_of_usize(stack_index(stack, pane));
            if stack.is_empty() {
                inputs.push.snap(to);
                inputs.push_pos = None;
            } else {
                let direction = tilo_ui_base::motion::Direction::of(to >= inputs.push.target());
                let timing = tilo_ui_base::motion::Drill::SPEC.timing(
                    direction,
                    self.layout_theme.motion(),
                    source,
                );
                inputs.push.set(to, timing, now);
                inputs.push_pos = inputs.push.is_moving(now).then(|| inputs.push.get(now));
            }
        }
        if let Some(binding) = self.pane_binding.as_mut() {
            binding.publish(&pane);
        }
        self.update_toggle_names();
        self.publish_open_states();
        self.animating = true;
        self.structure_dirty = true;
        self.invalidate();
    }

    fn refresh_content_back(&mut self) {
        let Some(back) = self.content_back.as_ref() else {
            return;
        };
        let can = (back.can_go_back)();
        self.set_can_go_back(can);
    }

    /// The parts a frame paints: laid out, and on screen inside the shell.
    fn painted_parts(&self) -> impl Iterator<Item = &dyn Widget> {
        self.laid_out_parts
            .iter()
            .filter_map(|part| self.part(*part))
            .filter(|part| part.paint_bounds().intersect(self.rect).area() > 0.0)
    }

    pub fn snap(&mut self) {
        {
            let mut inputs = self.inputs.borrow_mut();
            let layout = layout_of(self.geometry.sidebar);
            let open = inputs.sidebar.is_open(layout);
            inputs.sidebar.snap(open);
            let layout = inspector_layout_of(self.geometry.inspector);
            let open = inputs.inspector.is_open(layout);
            inputs.inspector.snap(open);
            let detent = inputs.sheet.detent();
            inputs.sheet.snap_to(detent);
            inputs.push_pos = None;
            let at = inputs.push.target();
            inputs.push.snap(at);
        }
        self.fade = tilo_ui_base::motion::Presence::opened();
        self.switch_fade = tilo_ui_base::motion::Presence::opened();
        self.content_fade = tilo_ui_base::motion::Presence::opened();
        self.switch_columns = [false; 4];
        self.animating = false;
        self.settling = false;
        self.slid = false;
        self.structure_dirty = true;
        self.invalidate();
    }

    fn update_toggle_names(&mut self) {
        let shortcut = HostShortcut::Sidebar
            .shortcut(self.conventions)
            .label(self.conventions);
        let sidebar_open = self.is_sidebar_open();
        let name = tilo_ui_base::i18n::I18nManager::lookup_plain(if sidebar_open {
            "window-toolbar-hide-sidebar"
        } else {
            "window-toolbar-show-sidebar"
        });
        self.toggle.set_accessible_name(&name);
        self.toggle
            .set_tooltip(Some(format!("{name} ({shortcut})")));
        let name = tilo_ui_base::i18n::I18nManager::lookup_plain(if self.is_inspector_open() {
            "window-toolbar-hide-inspector"
        } else {
            "window-toolbar-show-inspector"
        });
        self.inspector_toggle.set_accessible_name(&name);
        self.inspector_toggle.set_tooltip(Some(name));
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
        let (shown, control_only) = self.row_actions(column);
        (0..count)
            .filter(|index| !action_in_menu(shown, control_only, count, *index))
            .count()
    }

    /// What a column's row decides it stands: how many of its actions it keeps from the start
    /// (`None` until the first layout decides), and the one control it stands alone.
    fn row_actions(&self, column: Column) -> (usize, Option<usize>) {
        let count = at(&self.headers, column).actions.len();
        let inputs = self.inputs.borrow();
        let fit = inputs.fit.borrow();
        let part = at(&fit, column);
        (part.shown.unwrap_or(count).min(count), part.control_only)
    }

    /// Whether the action at `index` waits in the column's menu rather than standing in its row.
    fn action_in_menu(&self, column: Column, index: usize) -> bool {
        let count = at(&self.headers, column).actions.len();
        let (shown, control_only) = self.row_actions(column);
        action_in_menu(shown, control_only, count, index)
    }

    fn part_hidden(&self, part: Part) -> bool {
        match part {
            Part::Action(column, i) => self.action_in_menu(column, i),
            Part::More(column) => {
                self.inputs.borrow().actions_listed
                    || self.shown_actions(column) >= at(&self.headers, column).actions.len()
            }
            _ => false,
        }
    }

    /// A part of a push screen that is not in front: laid out while it
    /// slides, but it takes no input and is not exposed. A floating drawer
    /// is not modal, so nothing beside it is inert.
    fn part_inert(&self, part: Part) -> bool {
        let g = &self.geometry;
        // Both screens remain laid out during a push animation, but only the chosen screen is
        // operable.
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
            Part::StatusAction => Some(self.geometry.status_column()),
            Part::Band => None,
        }
    }

    fn column_theme(&self, theme: &Theme, column: Option<Column>) -> Theme {
        match column {
            Some(Column::Sidebar) => match self.geometry.sidebar {
                SidebarPlacement::Drawer => {
                    theme.on_ground(tilo_ui_base::chrome::panel_ground(theme))
                }
                _ => theme.in_region(tilo_ui_base::surface::Region::Sidebar),
            },
            Some(Column::Inspector) => match self.geometry.inspector {
                InspectorPlacement::Sheet | InspectorPlacement::Drawer => {
                    theme.on_ground(tilo_ui_base::chrome::panel_ground(theme))
                }
                _ => theme.in_region(tilo_ui_base::surface::Region::Inspector),
            },
            Some(Column::Overview | Column::Content) | None => theme.clone(),
        }
    }

    fn open_more_menus(&mut self) {
        let viewport = self.rect;
        for column in Column::ALL {
            let count = at(&self.headers, column).actions.len();
            let (shown, control_only) = self.row_actions(column);
            let header = at_mut(&mut self.headers, column);
            if !header.more.take_open_request() {
                continue;
            }
            let items = header
                .actions
                .iter()
                .enumerate()
                .filter(|(index, _)| action_in_menu(shown, control_only, count, *index))
                .map(|(index, action)| {
                    let node = action_node(&**action);
                    let name = node
                        .as_ref()
                        .and_then(|node| node.name.clone())
                        .unwrap_or_default();
                    let mut item = tilo_ui::widget::context_menu::MenuItem::selection(name)
                        .with_enabled(node.as_ref().is_none_or(|node| !node.state.disabled));
                    if let Some(checked) = node.as_ref().and_then(|node| node.state.checked) {
                        item = item.with_checked(checked);
                    }
                    let Some(button) = button_of(&**action) else {
                        return (index, item);
                    };
                    if let Some(icon) = button.glyph() {
                        item = item.with_icon(icon);
                    }
                    if let Some(key) = button.declared_key() {
                        item = item.with_shortcut(key);
                    }
                    if let Some(why) = button.why_disabled() {
                        item = item.with_description(why);
                    }
                    let item = item.with_destructive(
                        button.kind() == tilo_ui::widget::ButtonRole::Destructive,
                    );
                    let item = match button.menu_group() {
                        Some(group) => item.with_group(group),
                        None => item,
                    };
                    (index, item)
                })
                .collect();
            header.more.open_numbered(items, viewport);
            self.paint_dirty = true;
        }
    }

    /// Activate the action the person chose in a More menu: from the event
    /// that chose it, so its invocation leaves with that event.
    fn take_more_choices(
        &mut self,
        state: &mut tilo_ui_base::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        let mut acted = false;
        for column in Column::ALL {
            let header = at_mut(&mut self.headers, column);
            if let Some(row) = header.more.take_choice()
                && let Some(action) = header.actions.get_mut(row)
            {
                action.perform_action(&tilo_ui_base::a11y::Action::Activate, &[], state, path);
                acted = true;
            }
        }
        acted
    }

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
        if let (Some(field), Some(target)) = (self.filter.as_deref_mut(), target) {
            tilo_ui::widget::sync_filter(field, target);
        }
    }

    /// The person chose a destination in the sidebar or a row of the
    /// overview: a narrow window moves on to what they chose.
    fn follow_navigation(&mut self) -> bool {
        let from_sidebar = self
            .sidebar
            .as_deref_mut()
            .is_some_and(|sidebar| tilo_ui::widget::nav_list::take_navigation(sidebar));
        let from_rail = self
            .rail
            .as_deref_mut()
            .is_some_and(|rail| tilo_ui::widget::nav_list::take_navigation(rail));
        let from_overview = self
            .overview
            .as_deref_mut()
            .is_some_and(|overview| tilo_ui::widget::nav_list::take_navigation(overview));
        if from_sidebar {
            self.chose_in = Some(NavPane::Sidebar);
            self.active_hint = Some(if self.has_overview() {
                NavPane::Overview
            } else {
                NavPane::Content
            });
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
        if from_overview || from_rail {
            self.chose_in = Some(NavPane::Overview);
            self.active_hint = Some(NavPane::Content);
        }
        if (from_overview || from_rail)
            && !self.geometry.stack.is_empty()
            && !self.geometry.side_by_side
        {
            self.show(NavPane::Content);
        }
        from_sidebar || from_rail || from_overview
    }

    /// Advance the slides to this frame's time.
    fn tick(&mut self, theme: &Theme) {
        let now = tilo_ui_base::motion::frame_now();
        let motion = theme.motion();
        let sidebar_layout = layout_of(self.geometry.sidebar);
        let inspector_layout = inspector_layout_of(self.geometry.inspector);
        let stack_empty = self.geometry.stack.is_empty();
        let mut moving = false;
        // The poses this frame was laid out and painted with.
        let advanced = {
            let mut inputs = self.inputs.borrow_mut();
            let before = (
                inputs.sidebar.reveal(),
                inputs.inspector.reveal(),
                inputs.push_pos,
                inputs.sheet.drawn(),
            );
            moving |= inputs.sidebar.advance(sidebar_layout, now, motion);
            moving |= inputs.inspector.advance(inspector_layout, now, motion);
            moving |= inputs.sheet.advance(now);
            if stack_empty || !inputs.push.is_moving(now) {
                inputs.push_pos = None;
            } else {
                inputs.push_pos = Some(inputs.push.get(now));
                moving = true;
            }
            before
                != (
                    inputs.sidebar.reveal(),
                    inputs.inspector.reveal(),
                    inputs.push_pos,
                    inputs.sheet.drawn(),
                )
        };
        if self.switch_fade.is_moving(now) {
            self.paint_dirty = true;
            moving = true;
        } else if self.switch_columns.contains(&true) {
            self.switch_columns = [false; 4];
            self.paint_dirty = true;
        }
        if self.fade.is_moving(now) {
            self.paint_dirty = true;
            moving = true;
        }
        let slid = moving || advanced;
        if self.content_fade.is_moving(now) {
            self.paint_dirty = true;
            moving = true;
        }
        self.settling = false;
        if moving || advanced {
            self.invalidate();
            self.animating = true;
            self.slid |= slid;
        } else if self.animating {
            if std::mem::take(&mut self.slid) {
                self.structure_dirty = true;
            }
            self.invalidate();
            self.animating = false;
            self.settling = true;
        }
    }

    /// What the content shows changed (`NavShellProps::content_key`): what it shows now fades in
    /// where the one before it stood, on the theme's cross-fade, and lands at once for a person
    /// who asked for less motion. The layout is the new content's from this frame, so a page never
    /// waits for an old one to leave.
    pub(crate) fn content_replaced(&mut self, key: Option<String>) {
        let changed = key != self.content_key;
        self.content_key = key;
        if !changed {
            return;
        }
        let now = tilo_ui_base::motion::frame_now();
        let source = tilo_ui_base::motion::recent_input(now)
            .unwrap_or(tilo_ui_base::tokens::MotionSource::Action);
        // The content it shows now is what the window shows: on a narrow window, where the list
        // stands in front, it comes forward as a place chosen from that list does.
        if self.has_content() {
            self.show_from(NavPane::Content, source);
        }
        self.content_fade.snap(false);
        self.content_fade.show(
            true,
            tilo_ui_base::motion::CROSS_FADE,
            self.layout_theme.motion(),
            source,
            now,
        );
        if let Some(until) = self.content_fade.settles_at() {
            tilo_ui_base::motion::want_frames_until(until);
        }
        self.paint_dirty = true;
        self.invalidate();
    }

    fn fading_in(motion: tilo_ui_base::tokens::EffectiveMotion) -> tilo_ui_base::motion::Presence {
        let mut fade = tilo_ui_base::motion::Presence::closed();
        fade.show(
            true,
            tilo_ui_base::motion::CROSS_FADE,
            motion,
            tilo_ui_base::tokens::MotionSource::Pointer,
            tilo_ui_base::motion::frame_now(),
        );
        fade
    }

    fn refresh_geometry(&mut self) {
        // The class this pass's rect asks for, held through the hysteresis band: derived from
        // the rect every pass, never stored by an app or seeded by the host.
        let growth = f64::from(self.layout_theme.text_growth()).max(f64::from(f32::EPSILON));
        let tokens = self.layout_theme.tokens.metrics;
        let size_class = self.size_class.derive(self.rect.width() / growth, &tokens);
        let mut local = plan(&self.inputs.borrow(), self.rect.width(), self.rect.height());
        local.size_class = size_class;
        self.geometry = local.translated(Vec2::new(self.rect.x0, self.rect.y0));
        self.place_sheet();
    }

    /// The screen a new window opens on, from [`opening_pane`](layout::opening_pane): the first
    /// list of items — the overview where there is one, the content's own list where it is one (a
    /// mail window on the inbox, a Files window on the folder) or the empty state in place of
    /// it — else the sidebar (Settings' list).
    fn opening_pane(&self, stack: &[NavPane]) -> NavPane {
        stack
            .get(layout::opening_pane(&self.inputs.borrow(), stack))
            .copied()
            .unwrap_or(NavPane::Content)
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
            // The first layout opens on the stack's first screen unless the app chose one.
            let chosen = self.pane_chosen.then(|| self.inputs.borrow().pane);
            let keep = match (previous, chosen) {
                (None, None) => self.opening_pane(&stack),
                (None, Some(_)) => self.front_pane(),
                (Some(_), chosen) => self
                    .focused_pane()
                    .filter(|pane| self.chose_in != Some(*pane))
                    .or(self.active_hint)
                    .or(chosen)
                    .unwrap_or(NavPane::Content),
            };
            let fallback = self.content_pane();
            let mut inputs = self.inputs.borrow_mut();
            inputs.pane = if stack.is_empty() || stack.contains(&keep) {
                keep
            } else {
                fallback
            };
            inputs.push_pos = None;
            let at = inputs.push.target();
            inputs.push.snap(at);
            let pane = inputs.pane;
            drop(inputs);
            // A layout the person switched at a fixed size carries its columns from where they
            // stood.
            let resized = before.size != self.geometry.size;
            if switched && !resized {
                self.move_into_layout(before);
            }
            let planned = self.planned.take();
            if switched || planned != Some((stack, pane)) {
                self.structure_dirty = true;
            }
            self.update_toggle_names();
            self.invalidate();
        }
    }

    /// Carry the columns from where they stood into the layout a switch at one size made
    /// (`motion.durations.base`), so nothing jumps: into one column, the front screen slides to
    /// fill the window while the sidebar slides out under the leading edge; widening into a
    /// sidebar the person was not looking at slides it in.
    fn move_into_layout(&mut self, before: &NavGeometry) {
        let (w, h) = (self.rect.width(), self.rect.height());
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let reduce = self.layout_theme.reduce_motion;
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
                let i = f64_of_usize(stack_index(&after.stack, front));
                let last = f64_of_usize(after.stack.len().saturating_sub(1));
                let from = (i - (was.x0 - is.x0) / w).clamp(0.0, last);
                let now = tilo_ui_base::motion::frame_now();
                inputs.push.snap(from);
                let timing = tilo_ui_base::motion::Drill::SPEC.timing(
                    tilo_ui_base::motion::Direction::Enter,
                    self.layout_theme.motion(),
                    tilo_ui_base::tokens::MotionSource::Pointer,
                );
                inputs.push.set(i, timing, now);
                inputs.push_pos = inputs.push.is_moving(now).then_some(from);
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
            self.switch_fade = Self::fading_in(self.layout_theme.motion());
        }
        if carried.contains(&true) || faded.contains(&true) {
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

    fn apply_fit(&mut self) {
        let (fit, listed) = {
            let inputs = self.inputs.borrow();
            (Rc::clone(&inputs.fit), inputs.actions_listed)
        };
        let mut fit = fit.borrow_mut();
        let mut gave = false;
        for (part, header) in fit.iter_mut().zip(self.headers.iter_mut()) {
            let fitted = fit_of(header);
            gave |= part.capable != fitted.capable
                || part.compact_now != fitted.compact_now
                || part.primary != fitted.primary;
            if part.full.len() != fitted.capable.len() {
                part.full = vec![None; fitted.capable.len()];
                part.compact = false;
                part.shown = None;
                part.control_only = None;
                part.words_labelled = false;
            }
            keep_fitted(part, fitted);
            let count = header.actions.len();
            let shown = part.shown.unwrap_or(count);
            let standing = match part.control_only {
                Some(_) => 1,
                None => shown.min(count),
            };
            header.more.set_shown(!listed && standing < count);
            for (i, action) in header.actions.iter_mut().enumerate() {
                if !part.capable.get(i).copied().unwrap_or(false) {
                    continue;
                }
                let want = part.compact && !(part.words_labelled && Some(i) == part.words);
                if part.compact_now.get(i).copied() == Some(want) {
                    continue;
                }
                if let Some(button) = button_of_mut(&mut **action) {
                    button.set_compact_toolbar_presentation(want);
                    gave = true;
                    if let Some(now) = part.compact_now.get_mut(i) {
                        *now = want;
                    }
                }
            }
        }
        self.layout_dirty |= gave;
    }

    fn paint(&mut self, f: &mut Frame<'_>) {
        let g = self.geometry.clone();
        let fade = unit_f32(self.fade.progress(tilo_ui_base::motion::frame_now()));
        let content = unit_f32(
            self.content_fade
                .progress(tilo_ui_base::motion::frame_now()),
        );

        let inspector_region =
            g.inspector == InspectorPlacement::Docked && g.inspector_column.width() > 0.0;
        let sidebar_region = matches!(
            g.sidebar,
            SidebarPlacement::Docked | SidebarPlacement::Screen
        ) && g.sidebar_column.width() > 0.0;

        if self.built_has(Part::Rail) && g.rail.width() > 0.0 {
            let local = self.column_theme(f.theme, Some(Column::Overview));
            f.draw(self.part_mut(Part::Rail), &local);
        }
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
                tilo_ui_base::surface::region(
                    f.scene,
                    f.transform,
                    g.sidebar_column,
                    tilo_ui_base::surface::Region::Sidebar,
                    f.theme,
                );
            }
            if column == Column::Inspector && inspector_region {
                tilo_ui_base::surface::region(
                    f.scene,
                    f.transform,
                    g.inspector_column,
                    tilo_ui_base::surface::Region::Inspector,
                    f.theme,
                );
            }
            // What the content shows changed: the content it shows now arrives in place of the
            // one before it, fading in where that one stood.
            let arriving = column == Column::Content && content < 1.0;
            if arriving {
                let rect = g.column(Column::Content);
                f.scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    content,
                    f.transform,
                    &rect,
                );
            }
            self.paint_column(f, column);
            if arriving {
                f.scene.pop_layer();
            }
            if faded {
                f.scene.pop_layer();
            }
        }
        self.paint_rules(f.scene, f.transform, f.theme, &g);
        if self.resize_focused && self.sidebar_resize_handle().is_some() {
            tilo_ui::widget::paint_focus_ring(f.scene, f.transform, g.resize_handle, 2.0, f.theme);
        }

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
                self.paint_drawer_ground(f, column, rect);
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
        self.paint_status_action(f);
        self.paint_sync_status(f);
        self.paint_band(f);
    }

    fn paint_status_action(&mut self, f: &mut Frame<'_>) {
        if !self.built_has(Part::StatusAction) || self.part_hidden(Part::StatusAction) {
            return;
        }
        let local = self.column_theme(f.theme, self.part_column(Part::StatusAction));
        if let Some(button) = self.status_action.as_mut()
            && !button.get_rect().is_zero_area()
        {
            f.draw(Some(button as &mut dyn Widget), &local);
        }
    }

    /// The sync status in the header of the column History stands in (the one reaching the window's
    /// trailing edge), after its title and before its trailing controls, where the row fits.
    fn paint_sync_status(&mut self, f: &mut Frame<'_>) {
        self.status_span = Rect::ZERO;
        let g = &self.geometry;
        let column = g.status_column();
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
            .chain([Part::More(column), Part::StatusAction])
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
        if x1 > x0 {
            self.status_span = Rect::new(x0, row.y0, x1, row.y1);
        }
        self.sync_status
            .paint(f.scene, f.transform, f.text, f.theme, row, x0, x1);
    }

    /// Whether `part` is in the host tree this frame: only those were laid
    /// out, so only those paint.
    fn built_has(&self, part: Part) -> bool {
        self.laid_out_parts.contains(&part)
    }

    fn paint_column(&mut self, f: &mut Frame<'_>, column: Column) {
        let rect = self.geometry.column(column);
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        let local = self.column_theme(f.theme, Some(column));
        let clip = rect.intersect(self.rect);
        let alpha = if self.switch_columns[column.index()] {
            unit_f32(self.switch_fade.progress(tilo_ui_base::motion::frame_now()))
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
        for part in over {
            if self.built_has(part) && !self.part_hidden(part) {
                f.draw(self.part_mut(part), &local);
            }
        }
        f.scene.pop_layer();
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

    /// The hairline under the header row over the content columns, and the rule between the
    /// overview (or its rail) and the content; regions are told apart by tone, not lines.
    fn paint_rules(&self, scene: &mut Scene, transform: Affine, theme: &Theme, g: &NavGeometry) {
        for column in [Column::Overview, Column::Content, Column::Inspector] {
            let header = g.header(column);
            if header == Rect::ZERO || g.floating(column) {
                continue;
            }
            let local = self.column_theme(theme, Some(column));
            tilo_ui::widget::window_header::header_row::HeaderRowFace::in_row(
                header,
                self.inputs.borrow().metrics.row,
            )
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
                tilo_ui_base::surface::edge_on(theme.tokens.colors.surfaces.content),
                None,
                &rule,
            );
        }
    }
}

fn downcast<'a, T: Widget + 'static>(
    part: Option<&'a mut (dyn Widget + 'static)>,
) -> Option<&'a mut T> {
    part.and_then(|widget| widget.as_any_mut())
        .and_then(|any| any.downcast_mut::<T>())
}

struct Frame<'a> {
    scene: &'a mut Scene,
    transform: Affine,
    text: &'a mut TextRenderer,
    theme: &'a Theme,
    env: &'a tilo_ui_base::draw::DrawEnv<'a>,
}

impl Frame<'_> {
    fn draw(&mut self, widget: Option<&mut (dyn Widget + 'static)>, theme: &Theme) {
        if let Some(widget) = widget {
            tilo_ui_base::widget::draw_child(
                widget,
                self.scene,
                self.transform,
                self.text,
                theme,
                self.env,
            );
        }
    }
}

impl Widget for NavShell {
    fn on_nav_gesture(
        &mut self,
        dir: tilo_ui_base::gesture::NavDirection,
        state: &AppModel,
        path: &ModelPath,
    ) -> bool {
        self.nav_gesture(dir, state, path)
    }

    fn nav_availability(&self) -> tilo_ui_base::gesture::NavAvailability {
        self.nav_gesture_availability()
    }

    fn provide<'a>(&'a self, request: &mut tilo_ui_base::widget::capability::Request<'a, '_>) {
        if request.wants::<Self>() {
            request.provide::<Self>(self);
        } else if request.wants::<dyn tilo_ui_base::widget::controls::ActionControls>() {
            request.provide::<dyn tilo_ui_base::widget::controls::ActionControls>(self);
        } else if request.wants::<dyn WindowHeader>() {
            request.provide::<dyn WindowHeader>(self);
        } else if request.wants::<dyn tilo_ui::widget::resources::PublishesResources>() {
            request.provide::<dyn tilo_ui::widget::resources::PublishesResources>(self);
        } else if request.wants::<dyn tilo_ui_base::widget::ResourceOperations>() {
            request.provide::<dyn tilo_ui_base::widget::ResourceOperations>(self);
        } else if self.sidebar_search
            && request.wants::<dyn tilo_ui::widget::Filterable>()
            && let Some(sidebar) = &self.sidebar
        {
            sidebar.provide(request);
        } else if !request.wants::<dyn tilo_ui_base::widget::Decorated>() {
            self.content.provide(request);
        }
    }

    fn provide_mut<'a>(
        &'a mut self,
        request: &mut tilo_ui_base::widget::capability::RequestMut<'a, '_>,
    ) {
        if request.wants::<dyn tilo_ui_base::widget::controls::ActionControls>() {
            request.provide::<dyn tilo_ui_base::widget::controls::ActionControls>(self);
        } else if request.wants::<dyn WindowHeader>() {
            request.provide::<dyn WindowHeader>(self);
        } else if request.wants::<dyn tilo_ui::widget::resources::PublishesResources>() {
            request.provide::<dyn tilo_ui::widget::resources::PublishesResources>(self);
        } else if request.wants::<dyn tilo_ui_base::widget::ResourceOperations>() {
            request.provide::<dyn tilo_ui_base::widget::ResourceOperations>(self);
        } else if self.sidebar_search
            && request.wants::<dyn tilo_ui::widget::Filterable>()
            && let Some(sidebar) = &mut self.sidebar
        {
            sidebar.provide_mut(request);
        } else if !request.wants::<dyn tilo_ui_base::widget::Decorated>() {
            self.content.provide_mut(request);
        }
    }

    fn native_input_snapshot(
        &self,
        state: &AppModel,
        path: &ModelPath,
    ) -> Option<serde_json::Value> {
        self.content.native_input_snapshot(state, path)
    }

    fn set_platform_conventions(&mut self, conventions: PlatformConventions) {
        self.conventions = conventions;
        self.update_toggle_names();
        self.for_each_owned_mut(|child| child.set_platform_conventions(conventions));
    }

    fn set_interaction_environment(
        &mut self,
        environment: tilo_ui_base::interaction::InteractionEnvironment,
    ) {
        self.hear_input(environment);
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
        self.publish_notice();
    }

    fn settle(&mut self, edits: &mut tilo_ui_base::widget::Edits<'_>, path: &automerge::ObjId) {
        // A command chosen from a header's menu by assistive technology has no pointer event to
        // carry it, so the settle pass runs it.
        self.take_more_choices(edits, path);
        self.for_each_owned_mut(|child| child.settle(edits, path));
        self.sync_status.settle(edits, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        self.finish_part_edit();
        let mut changed = self.take_requests();
        self.for_each_owned_mut(|child| changed |= child.update(state, path));
        changed |= self.follow_navigation();
        changed |= self.notice.expire();
        self.publish_notice();
        if self.animating || self.settling {
            tilo_ui_base::widget::note_layout_moved();
        }
        changed
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.finish_part_edit();
        self.apply_pending();
        self.tell_parts_input();
        self.refresh_part_inputs();
        *self.layout_theme = theme.clone();
        let size = if self.rect.width() > 0.0 {
            (self.rect.width(), self.rect.height())
        } else {
            (
                f64::from(theme.tokens.metrics.compact_breakpoint) * 4.0,
                f64::from(theme.tokens.metrics.compact_breakpoint),
            )
        };
        let (metrics, adaptive) = self.sidebar_metrics(theme);
        let (parts, sidebar_width) = {
            let mut inputs = self.inputs.borrow_mut();
            (inputs.metrics, inputs.adaptive) = (metrics, adaptive);
            let g = plan(&inputs, size.0, size.1);
            if self.last_stack.is_none() {
                self.planned = Some((g.stack.clone(), inputs.pane));
            }
            let parts = g.visible_parts(&inputs);
            inputs.parts.clone_from(&parts);
            (parts, g.body(Column::Sidebar).width())
        };
        self.follow_sidebar_width(sidebar_width);
        self.update_toggle_names();
        self.apply_bar_controls(&parts);
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
        self.laid_out_parts = parts;
        self.built = true;
        self.retired_status_actions.clear();
        self.structure_dirty = false;
        self.bodies_swapped = false;
        self.content_shown_built = self.content_shown;
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
        let before = self
            .geometry
            .translated(Vec2::new(-self.rect.x0, -self.rect.y0));
        self.rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
        let stale = self.replan(&before);
        self.inputs.borrow_mut().held = Held::of(&self.geometry);
        self.follow_sidebar_width(self.geometry.body(Column::Sidebar).width());
        let child_origin = Point::new(self.rect.x0, self.rect.y0);
        let child_nodes = taffy.children(node).unwrap_or_default();
        let parts = self.laid_out_parts.clone();
        for (part, child) in parts.iter().zip(child_nodes) {
            if let Some(widget) = self.part_mut(*part) {
                widget.apply_layout(taffy, child, child_origin);
            }
        }
        self.apply_fit();
        let mirrored = self.mirror_header_counts();
        let covered = self.mirror_own_page_titles();
        self.invalidation.acknowledge_applied();
        self.layout_dirty =
            stale.then(|| self.invalidation.invalidate()).is_some() || mirrored || covered;
        self.apply_open_states();
        self.publish_open_states();
        let visible = self.geometry.visible_parts(&self.inputs.borrow());
        if visible != self.laid_out_parts || !self.pending.is_empty() {
            self.structure_dirty = true;
        }
    }

    fn layout_needs_measure(&self) -> bool {
        let mut dirty = self.invalidation.is_pending();
        // Hidden headers and controls may never have been measured, so they must not invalidate an
        // unrelated visible pane on every scroll; revealing a part invalidates the shell.
        for part in &self.laid_out_parts {
            if let Some(child) = self.part(*part) {
                dirty |= child.layout_needs_measure();
            }
        }
        dirty
    }

    /// Its own invalidation and the parts it laid out; a part it hid is laid out when shown.
    fn is_layout_dirty(&self) -> bool {
        let part_dirty = |part: &_| self.part(*part).is_some_and(|c| c.is_layout_dirty());
        self.layout_dirty
            || self.invalidation.is_pending()
            || self.laid_out_parts.iter().any(part_dirty)
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        self.paint(&mut Frame {
            scene,
            transform,
            text: text_renderer,
            theme,
            env: &env,
        });
        self.tick(theme);
    }

    fn draw_overlay(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        for child in self.children_mut() {
            child.draw_overlay(&mut tilo_ui_base::draw::DrawCtx::new(
                scene,
                transform,
                text_renderer,
                theme,
                env.clone(),
            ));
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut tilo_ui_base::widget::EventCtx<'_>) {
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
        // Always listed, with or without text: the status changes by paint
        // alone, so its coming and going must not change the tree's shape.
        children.push(&self.sync_status);
        children
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        for part in self.exposed_parts() {
            if let Some(widget) = self.exposed(part) {
                visitor(widget);
            }
        }
        visitor(&self.sync_status);
    }

    fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        for part in self.exposed_parts() {
            if let Some(widget) = self.part(part) {
                visitor(widget);
            }
        }
        visitor(&self.sync_status);
        true
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
            sync_status,
            status_action,
            band,
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
        let mut filter = filter.as_deref_mut().map(|f| f as &mut dyn Widget);
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
        let mut toggle = Some(&mut **toggle as &mut dyn Widget);
        let mut back = Some(&mut **back as &mut dyn Widget);
        let mut inspector_toggle = Some(&mut **inspector_toggle as &mut dyn Widget);
        let mut history_open = Some(&mut **history_open as &mut dyn Widget);
        let mut history_close = Some(&mut **history_close as &mut dyn Widget);
        let mut status_action = status_action.as_mut().map(|b| b as &mut dyn Widget);
        let mut band = band.as_deref_mut().map(|w| w as &mut dyn Widget);
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
                Part::StatusAction => status_action.take(),
                Part::Band => band.take(),
            };
            if let Some(widget) = next {
                out.push(widget);
            }
        }
        out.push(sync_status);
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

    /// Each column's header row as the last layout left it: what its title says against what
    /// the row gave it, and what its actions kept of their labels and their place in the row.
    fn layout_info(&self) -> Option<tilo_ui_base::widget::LayoutInfo> {
        let inputs = self.inputs.borrow();
        let fit = inputs.fit.borrow();
        let mut rows: [Option<tilo_ui_base::widget::HeaderRowLayout>; 4] = [None; 4];
        for (index, (row, part)) in rows.iter_mut().zip(fit.iter()).enumerate() {
            *row = part
                .header
                .map(|facts| tilo_ui_base::widget::HeaderRowLayout {
                    column: Column::ALL[index].key(),
                    title_needs: tilo_ui_base::num::f32_of_f64(facts.title_needs),
                    title_has: tilo_ui_base::num::f32_of_f64(facts.title_has),
                    free_beside_title: tilo_ui_base::num::f32_of_f64(facts.free_beside_title),
                    title_opens: self
                        .headers
                        .get(index)
                        .is_none_or(|header| header.title.opens_within(facts.title_has)),
                    room_reads: self.headers.get(index).is_none_or(|header| {
                        header
                            .title
                            .reads_within(facts.title_has + facts.free_beside_title)
                    }),
                    labelled_action: facts.labelled_action,
                    primary_shown: facts.primary_shown,
                });
        }
        let m = inputs.metrics;
        let beside = self.beside_sidebar();
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a window's width in logical pixels, far inside f32's range"
        )]
        let (beside_rem, beside_width) =
            ((beside / f64::from(m.rem.max(1.0))) as f32, beside as f32);
        Some(tilo_ui_base::widget::LayoutInfo::Shell(
            tilo_ui_base::widget::ShellLayout {
                sidebar: match self.geometry.sidebar {
                    SidebarPlacement::None => tilo_ui_base::widget::SidebarPosition::None,
                    SidebarPlacement::Docked => tilo_ui_base::widget::SidebarPosition::Docked,
                    SidebarPlacement::Drawer => tilo_ui_base::widget::SidebarPosition::Over,
                    SidebarPlacement::Screen => tilo_ui_base::widget::SidebarPosition::Alone,
                },
                beside: tilo_ui_base::widget::WidthClass::of(beside_rem),
                needs: inputs.content_beside_sidebar_class(),
                beside_width,
                rows: tilo_ui_base::widget::HeaderRows { rows },
            },
        ))
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        (self.resize.is_some() || self.geometry.resize_handle.contains(point))
            .then_some(winit::window::CursorIcon::ColResize)
            .or_else(|| self.sheet_cursor(point))
    }

    /// A finger on the sidebar's edge drags it; it never scrolls.
    fn claims_raw_touch(&self, point: Point) -> bool {
        self.sidebar_resize_touch_band().contains(point) || self.sheet_claims_touch(point)
    }

    /// The window's group, keyed by the shell's id so what stands under a titled column keeps its
    /// identity on the agent surface while titles come and go; the titles are its headings.
    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        use tilo_ui_base::{a11y::AccessNode, a11y::Role};
        let key = self.id.as_deref().unwrap_or("nav-shell");
        let resize = self.sidebar_resize_handle().map(|handle| {
            let target = self.sidebar_resize_target();
            handle
                .accessibility()
                .with_name(tilo_ui_base::i18n::widget_format(
                    "widget-sidebar-resize",
                    &[],
                ))
                .with_action_target(target.clone())
                .with_hashed_key(&target)
                .with_bounds(self.geometry.resize_handle)
        });
        Some(
            AccessNode::new(Role::Group)
                .with_semantic_key(format!("{key}.window"))
                .with_children(
                    resize
                        .into_iter()
                        .chain(self.sheet_accessibility())
                        .chain(self.more_accessibility())
                        .collect(),
                ),
        )
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &tilo_ui_base::a11y::Action,
        state: &mut tilo_ui_base::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        if target == self.sheet_target() {
            return self.perform_sheet_action(action);
        }
        if let Some(ran) = self.more_action(target, action, state, path) {
            return ran;
        }
        if target != self.sidebar_resize_target() || self.sidebar_resize_handle().is_none() {
            return false;
        }
        if matches!(action, tilo_ui_base::a11y::Action::Focus) {
            self.resize_focused = true;
            self.invalidate();
            return true;
        }
        self.step_sidebar_resize(action)
    }

    fn set_focus(&mut self, focused: bool) {
        if !focused {
            self.inputs.borrow_mut().sheet.set_focused(false);
        }
        if !focused && self.resize_focused {
            self.resize_focused = false;
            self.invalidate();
        }
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        let mut earliest = self.notice.deadline();
        self.for_each_owned(|child| {
            if let Some(deadline) = child.next_wake_deadline() {
                earliest = Some(earliest.map_or(deadline, |old| old.min(deadline)));
            }
        });
        earliest
    }

    fn serve_deadline(&mut self, now: Instant) -> bool {
        let mut served = false;
        self.for_each_owned_mut(|child| served |= child.serve_deadline(now));
        served
    }

    fn needs_animation_frame(&self) -> bool {
        // Hidden drawers retain their controls, but their entrances cannot advance until the drawer
        // is painted; only painted parts ask for frames, while background wakes still visit every
        // owned part.
        self.animating
            || self.settling
            || self.fade.is_moving(tilo_ui_base::motion::frame_now())
            || self
                .content_fade
                .is_moving(tilo_ui_base::motion::frame_now())
            || self.painted_parts().any(Widget::needs_animation_frame)
    }

    /// Only for what the next sync would change: a bound title or open state that moved, or a Back
    /// the page stack now answers differently. A binding alone asks nothing.
    fn needs_idle_sync(&self) -> bool {
        let open_moved = |state: &Option<OpenBinding>| {
            state
                .as_ref()
                .is_some_and(|state| state.wanted.is_some() || state.binding.changed())
        };
        let mut needed = self.animating
            || self
                .content_back
                .as_ref()
                .is_some_and(|back| (back.can_go_back)() != self.inputs.borrow().content_back)
            || self
                .headers
                .iter()
                .any(|header| header.binding.as_ref().is_some_and(LocalBinding::changed))
            || open_moved(&self.sidebar_state)
            || open_moved(&self.inspector_state);
        self.for_each_owned(|child| needed |= child.needs_idle_sync());
        needed
    }

    fn needs_redraw(&self) -> bool {
        self.paint_dirty
            || self.status_dirty
            || self.animating
            || self.settling
            || self.fade.is_moving(tilo_ui_base::motion::frame_now())
            || self.painted_parts().any(Widget::needs_redraw)
    }

    fn mark_drawn(&mut self) {
        self.paint_dirty = false;
        self.status_dirty = false;
        self.status_drawn = self.sync_status.get_rect();
        self.for_each_owned_mut(|child| child.mark_drawn());
        // The shell paints its titles itself, so they learn here what it
        // painted: as children they would otherwise ask for a frame forever.
        for header in self.headers.iter_mut() {
            header.title.mark_drawn();
        }
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        if self.paint_dirty
            || self.animating
            || self.fade.is_moving(tilo_ui_base::motion::frame_now())
        {
            regions.push(self.rect);
        } else {
            self.for_each_owned(|child| child.collect_damage(regions));
            if self.status_dirty {
                if self.status_span.is_zero_area() {
                    regions.push(self.rect);
                } else {
                    regions.extend(
                        [self.status_drawn, self.sync_status.get_rect()]
                            .into_iter()
                            .filter(|rect| !rect.is_zero_area()),
                    );
                }
            }
        }
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.take_requests();
        let mut dirty = std::mem::take(&mut self.structure_dirty)
            || self.bodies_swapped
            || self.content_column_changed();
        self.for_each_owned_mut(|child| dirty |= child.take_structure_dirty());
        dirty
    }

    /// The shell's own changes (a column shown or hidden, a header's part swapped) rebuild; a
    /// column's body swapped for another is put in place ([`Self::swap_bodies`]); what its parts
    /// describe (a column's content changed in place) stays described.
    fn take_structure_change(&mut self) -> StructureChange {
        self.take_requests();
        let mut change = StructureChange::of(
            std::mem::take(&mut self.structure_dirty) || self.content_column_changed(),
        );
        if self.bodies_swapped {
            change = change.join(StructureChange::Children);
        }
        let laid_out = self.exposed_parts();
        for part in self.all_parts() {
            if let Some(widget) = self.part_mut(part) {
                let part_change = widget.take_structure_change();
                if laid_out.contains(&part) {
                    change = change.join(part_change);
                }
            }
        }
        change
    }

    /// The parts the shell lays out carry out their own changes, those of a
    /// screen not in front too: they stand in the tree while it slides.
    fn remount_children(&mut self, remount: &mut tilo_ui_base::widget::Remount<'_>) -> bool {
        if self.bodies_swapped && !self.swap_bodies(remount) {
            return false;
        }
        let mut carried = true;
        for part in self.exposed_parts() {
            if let Some(widget) = self.part_mut(part) {
                carried &= widget.remount_children(remount);
            }
        }
        carried && self.sync_status.remount_children(remount)
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

/// What a header's actions can do as they stand now: which give up their label when the row is
/// short, which have already done so, where the row's menu begins, the one action the row never
/// gives up, and the controls it hosts among them.
struct HeaderFit {
    capable: Vec<bool>,
    compact_now: Vec<bool>,
    menu_from: Option<usize>,
    /// The app's own primary action, which keeps its words longest.
    words: Option<usize>,
    primary: Option<usize>,
    controls: Vec<usize>,
}

/// Whether action `index` of a row of `count` action waits in its menu: a row that stands its
/// control alone names every button there, and one that keeps `shown` actions names those after
/// them.
fn action_in_menu(shown: usize, control_only: Option<usize>, count: usize, index: usize) -> bool {
    match control_only {
        Some(control) => index != control,
        None => index >= shown.min(count),
    }
}

/// [`HeaderFit`] of `header`'s actions as they stand now.
/// Keep `part`'s reading of `fitted`: what the header's actions are, and which of them the row
/// never gives up.
fn keep_fitted(part: &mut ColumnFit, fitted: HeaderFit) {
    part.capable = fitted.capable;
    part.compact_now = fitted.compact_now;
    part.menu_from = fitted.menu_from;
    part.words = fitted.words;
    part.primary = fitted.primary;
    part.controls = fitted.controls;
}

fn fit_of(header: &Header) -> HeaderFit {
    let capable: Vec<bool> = header
        .actions
        .iter()
        .map(|action| button_of(&**action).is_some_and(Button::shrinks_to_icon))
        .collect();
    let compact_now: Vec<bool> = header
        .actions
        .iter()
        .map(|action| button_of(&**action).is_some_and(Button::is_shown_compact))
        .collect();
    let menu_from = header
        .actions
        .iter()
        .position(|action| button_of(&**action).is_some_and(Button::is_menu_only));
    // The action the app itself marks primary — the one that keeps its words while the row's
    // secondary actions give theirs — and, where the app marks none, the action the row never
    // gives up: the first that stands in the row rather than in its menu.
    let words = header.actions.iter().position(|action| {
        button_of(&**action)
            .is_some_and(|button| button.role_of() == ButtonRole::Primary && !button.is_menu_only())
    });
    let primary = words.or_else(|| {
        header
            .actions
            .iter()
            .position(|action| button_of(&**action).is_none_or(|button| !button.is_menu_only()))
    });
    let controls: Vec<usize> = header
        .actions
        .iter()
        .enumerate()
        .filter_map(|(index, action)| button_of(&**action).is_none().then_some(index))
        .collect();
    HeaderFit {
        capable,
        compact_now,
        menu_from,
        words,
        primary,
        controls,
    }
}

/// The button a header action is: the action itself, the control under the view a decorator
/// wrapped around it, or the one it shows as its face; the row fits it by its label, icon or
/// faces, and its menu lists it by glyph and key.
pub fn button_of(action: &dyn Widget) -> Option<&Button> {
    if let Some(button) = action.as_any().and_then(|any| any.downcast_ref::<Button>()) {
        return Some(button);
    }
    if let Some(inner) =
        tilo_ui_base::widget::capability::capability::<dyn tilo_ui_base::widget::Decorated>(action)
    {
        return button_of(inner.decorated());
    }
    action
        .children()
        .into_iter()
        .find_map(|child| child.as_any().and_then(|any| any.downcast_ref::<Button>()))
}

/// The accessibility node a header action speaks with: its own, or the one of the control it
/// shows as, for an action wrapped in a view of the window's state (which is presentational and
/// says nothing itself).
fn action_node(action: &dyn Widget) -> Option<tilo_ui_base::a11y::AccessNode> {
    if let Some(node) = action.accessibility() {
        return Some(node);
    }
    action.children().into_iter().find_map(action_node)
}

/// [`button_of`], to change how it shows.
fn button_of_mut(action: &mut dyn Widget) -> Option<&mut Button> {
    // The lookup hands back a pointer into the tree `action` owns, so each step's borrow of it
    // ends before the next begins (`capability_mut` borrows the whole action for its answer).
    let button = button_mut(action)?;
    // SAFETY: `button` points at the `Button` inside `action`'s own tree, which `action`'s
    // exclusive borrow keeps alive and unaliased for as long as the answer is used.
    Some(unsafe { &mut *button })
}

/// The `Button` inside `action`, by pointer: the action itself, the control a decorator wrapped
/// around it, or the one it shows as its face.
fn button_mut(action: &mut dyn Widget) -> Option<*mut Button> {
    if let Some(button) = action
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<Button>())
    {
        return Some(std::ptr::from_mut(button));
    }
    if let Some(inner) = tilo_ui_base::widget::capability::capability_mut::<
        dyn tilo_ui_base::widget::DecoratedMut,
    >(action)
        && let Some(button) = button_mut(inner.decorated_mut())
    {
        return Some(button);
    }
    action.children_mut().into_iter().find_map(|child| {
        child
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<Button>())
            .map(std::ptr::from_mut)
    })
}

/// Whether `action` is the button that opens the app's search, which the window's bar replaces.
fn is_app_search(action: &dyn Widget) -> bool {
    action.test_id() == Some(tilo_ui::widget::search_request::APP_SEARCH_BUTTON_ID)
}

/// Whether `body` shows, inside `region`, a text of its own that is the page's title and says
/// `title`: what a bar naming the page would repeat.
fn page_title_shown(body: &dyn Widget, region: Rect) -> Option<TitleShown> {
    fn walk(widget: &dyn Widget, clip: Rect) -> Option<TitleShown> {
        // An item opened in place among its list's rows titles that item, never the page: the
        // column keeps its own title.
        if widget.is_item_opened_in_place() {
            return None;
        }
        let clip = widget
            .child_clip_rect()
            .map_or(clip, |child_clip| clip.intersect(child_clip));
        if let Some(own) = widget.own_page_title() {
            let rect = widget.get_rect();
            let shown = rect.intersect(clip);
            if shown.width() > 0.0 && shown.height() > 0.0 {
                return Some(TitleShown {
                    rect,
                    words: own,
                    heading: widget.page_title_is_heading(),
                });
            }
        }
        let mut found = None;
        widget.for_each_child(&mut |child| {
            if found.is_none() {
                found = walk(child, clip);
            }
        });
        found
    }
    walk(body, region)
}

/// The page's own title as the content shows it: where it stands, the words it says, and whether
/// it is a heading assistive technology reads as the page's name or only the field the person
/// edits it in.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TitleShown {
    pub(crate) rect: Rect,
    pub(crate) words: String,
    pub(crate) heading: bool,
}

fn same_words(a: &str, b: &str) -> bool {
    let words = |text: &str| -> Vec<String> {
        text.split_whitespace()
            .map(str::to_lowercase)
            .collect::<Vec<_>>()
    };
    words(a) == words(b)
}

/// Controls in a column the shell is not showing (a hidden sidebar, a closed drawer, a
/// pushed-away screen) still belong to the window: ownership, invocations and availability reach
/// every part the shell owns.
impl tilo_ui::widget::resources::PublishesResources for NavShell {}

impl tilo_ui_base::widget::ResourceOperations for NavShell {}

impl tilo_ui_base::widget::controls::ActionControls for NavShell {
    fn action_control_bindings(&self) -> Vec<tilo_ui_base::widget::ControlBindingMetadata> {
        tilo_ui_base::widget::controls::children_action_control_bindings(self)
    }
    fn attach_controls(
        &mut self,
        domain: &tilo_ui_base::ControlDomain,
        attachment: tilo_ui_base::control_domain::DomainAttachment,
    ) {
        self.for_each_owned_mut(|child| {
            tilo_ui_base::widget::controls::attach_controls(child, domain, attachment);
        });
    }

    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        let mut taken = None;
        self.for_each_owned_mut(|child| {
            if taken.is_none() {
                taken = tilo_ui_base::widget::controls::take_control_invocation(child);
            }
        });
        taken
    }

    fn on_control_invocation_result(
        &mut self,
        address: &tilo_actions::ActionAddress,
        result: &Result<tilo_ui_base::widget::ResourceOperationStart, tilo_actions::ApiError>,
    ) {
        self.for_each_owned_mut(|child| {
            if !tilo_ui_base::widget::component_walk::has_separate_document_owner(child) {
                tilo_ui_base::widget::controls::on_control_invocation_result(
                    child, address, result,
                );
            }
        });
    }

    fn has_action_control_bindings(&self) -> bool {
        tilo_ui_base::widget::controls::children_have_action_control_bindings(self)
    }

    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        self.for_each_owned_mut(|child| {
            tilo_ui_base::widget::controls::refresh_control_action_availability(child, allowed);
        });
    }

    fn has_pending_control_invocation(&self) -> bool {
        let mut pending = false;
        self.for_each_owned(|child| {
            pending |= tilo_ui_base::widget::controls::has_pending_control_invocation(child);
        });
        pending
    }
}

mod component;
pub use component::NavShellProps;

impl NavShell {
    /// The commands behind each column's "…" while its menu is shut: what a row did not give
    /// room is named there for a screen reader, an agent and a tool that presses it by name,
    /// exactly as it is once the menu opens. They stand at the button they wait behind, since
    /// they are drawn nowhere until it opens.
    fn more_accessibility(&self) -> Vec<tilo_ui_base::a11y::AccessNode> {
        use tilo_ui_base::a11y::{AccessNode, AccessState, Role};
        let mut nodes = Vec::new();
        for column in Column::ALL {
            let header = at(&self.headers, column);
            if header.more.is_open() || !header.more.is_shown() {
                continue;
            }
            let count = header.actions.len();
            let (shown, control_only) = self.row_actions(column);
            for (index, action) in header.actions.iter().enumerate() {
                if !action_in_menu(shown, control_only, count, index) {
                    continue;
                }
                let Some(node) = action_node(&**action) else {
                    continue;
                };
                let Some(name) = node.name.clone() else {
                    continue;
                };
                let mut item = AccessNode::new(Role::MenuItem)
                    .with_name(name)
                    .with_action_target(format!("{MORE_TARGET}{}:{index}", column.key()))
                    .with_bounds(header.more.rect())
                    .with_position(index.saturating_add(1), count)
                    .with_state(AccessState {
                        // It is drawn nowhere until the menu opens, so it takes the keyboard
                        // nowhere; a reader, an agent and a press by name reach it all the same.
                        focusable: false,
                        disabled: node.state.disabled,
                        checked: node.state.checked,
                        ..Default::default()
                    });
                item.action.clone_from(&node.action);
                item.description.clone_from(&node.description);
                nodes.push(item);
            }
        }
        nodes
    }

    /// Run the command `target` names behind a column's "…", as pressing the row of its open
    /// menu runs it: `None` when the target names no such command.
    fn more_action(
        &mut self,
        target: &str,
        action: &tilo_ui_base::a11y::Action,
        state: &mut tilo_ui_base::widget::Edits<'_>,
        path: &ModelPath,
    ) -> Option<bool> {
        let (column, index) = target.strip_prefix(MORE_TARGET)?.split_once(':')?;
        let column = Column::ALL
            .into_iter()
            .find(|known| known.key() == column)?;
        let index = index.parse::<usize>().ok()?;
        let count = at(&self.headers, column).actions.len();
        let (shown, control_only) = self.row_actions(column);
        if !action_in_menu(shown, control_only, count, index) {
            return None;
        }
        let header = self.headers.get_mut(column.index())?;
        if header.more.is_open() {
            return None;
        }
        let action_widget = header.actions.get_mut(index)?;
        let ran = action_widget.perform_action(action, &[], state, path);
        if ran {
            self.invalidate();
        }
        Some(ran)
    }

    fn region_size() -> Size<taffy::Dimension> {
        Size::from_percent(1.0, 1.0)
    }

    /// What the shell itself answers: the sidebar shortcut, Escape, Ctrl+F, a scrim press and the
    /// resize drag; returns whether it consumed `event`.
    fn handle_own_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut tilo_ui_base::widget::EventCtx<'_>,
    ) -> bool {
        if self.sheet_event(event, ctx) {
            return true;
        }
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
                if self.resize.is_some() && self.edge.touch.is_none() {
                    self.end_sidebar_resize();
                    return true;
                }
                false
            }
            WindowEvent::Touch(touch) => self.handle_resize_touch(touch, ctx),
            _ => false,
        }
    }

    fn handle_key(&mut self, key: &Key, ctx: &mut tilo_ui_base::widget::EventCtx<'_>) -> bool {
        if self.resize_focused {
            let step = match key {
                Key::Named(NamedKey::ArrowLeft) => Some(tilo_ui_base::a11y::Action::Decrement),
                Key::Named(NamedKey::ArrowRight) => Some(tilo_ui_base::a11y::Action::Increment),
                _ => None,
            };
            if let Some(step) = step {
                self.step_sidebar_resize(&step);
                return true;
            }
        }
        let modifiers = ctx.modifiers.state();
        let command = self.conventions.command_modifier_down(modifiers);
        if command && let Key::Character(c) = key {
            if HostShortcut::resolve(self.conventions, c, modifiers.shift_key())
                == Some(HostShortcut::Sidebar)
                && self.has_sidebar()
            {
                self.toggle_by_key(Column::Sidebar);
                return true;
            }
            if c.eq_ignore_ascii_case("f") && !modifiers.shift_key() {
                let index = self
                    .laid_out_parts
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
                        self.toggle_by_key(Column::Sidebar);
                    }
                    *ctx.focus_target = Some(node);
                    return true;
                }
            }
        }
        if *key == Key::Named(NamedKey::Escape) {
            if self.back() {
                return true;
            }
        }
        false
    }

    fn handle_press(&mut self) -> bool {
        if self.resize_focused {
            self.resize_focused = false;
            self.invalidate();
        }
        let p = self.last_cursor;
        let g = &self.geometry;
        if g.resize_handle.contains(p) {
            self.resize = Some(Resize {
                start_x: p.x,
                start_width: g.sidebar_column.width(),
            });
            return true;
        }
        if let Some(column) = g.open_drawer()
            && (column == Column::Sidebar || g.inspector == InspectorPlacement::Drawer)
            && !g.column(column).contains(p)
            && !g.window_row_contains(p)
            && !g.band.contains(p)
        {
            if column == Column::Inspector {
                self.toggle_inspector();
            } else {
                self.toggle_sidebar();
            }
            return true;
        }
        // A press anywhere is the person moving on from the choice they made in a list; the
        // press on a row that chooses again records it afresh.
        self.chose_in = None;
        if g.content_column.contains(p) {
            self.active_hint = Some(NavPane::Content);
        } else if g.overview_column.contains(p) || g.rail.contains(p) {
            self.active_hint = Some(NavPane::Overview);
        }
        false
    }

    /// The rect a pointer event over `part` lands in.
    fn part_area(&self, part: Part) -> Rect {
        let g = &self.geometry;
        match part {
            Part::Toggle => g.toggle,
            Part::Back => g.back,
            Part::InspectorToggle => g.inspector_toggle,
            Part::History(_) => g.history,
            Part::StatusAction => self
                .status_action
                .as_ref()
                .map_or(Rect::ZERO, Widget::get_rect),
            Part::Filter => g.filter,
            Part::Controls => g.controls,
            Part::Rail => g.rail,
            Part::Band => g.band,
            Part::Title(column)
            | Part::Leading(column)
            | Part::Action(column, _)
            | Part::More(column) => g.header(column),
            Part::Body(column) => g.body(column).union(g.column(column)),
        }
    }

    /// Route `event` to the live parts, front to back: a part whose overlay holds the pointer
    /// (a popover a header button owns) hears pointer events first, then the part under the
    /// pointer, so a release or wheel lands where it happened.
    fn route_event_to_children(
        &mut self,
        event: &WindowEvent,
        ctx: &mut tilo_ui_base::widget::EventCtx<'_>,
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
            .laid_out_parts
            .iter()
            .copied()
            .zip(child_nodes)
            .rev()
            .filter(|(part, _)| !self.part_inert(*part) && !self.part_hidden(*part))
            .collect();
        // A part holding the pointer (a drag it started, crossing into a
        // neighbour's column) takes the pointer alone until it lets go.
        if pointer || matches!(event, WindowEvent::CursorMoved { .. }) {
            if let Some(&(part, node)) = order
                .iter()
                .find(|(part, _)| self.part(*part).is_some_and(|w| w.wants_pointer_capture()))
            {
                let path = ctx.path;
                if let Some(widget) = self.part_mut(part) {
                    widget.handle_event(event, &mut ctx.reborrow(path, node));
                }
                return;
            }
        }
        if pointer {
            let p = self.last_cursor;
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
            order.compact_sort_by_key(|(part, _)| rank(part));
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

/// What a navigation shell's tests read of it from outside the module.
#[cfg(feature = "test-support")]
impl NavShell {
    /// The toggle that folds the sidebar.
    #[must_use]
    pub fn test_toggle(&self) -> &Button {
        &self.toggle
    }

    /// Where the incoming page's frame stands, as a push the person is in the middle of.
    pub fn test_set_push_pos(&mut self, at: Option<f64>) {
        self.inputs.borrow_mut().push_pos = at;
    }

    /// The part `part` names, if the shell shows it.
    #[must_use]
    pub fn test_part(&self, part: Part) -> Option<&dyn Widget> {
        self.part(part)
    }

    /// Whether `part` is shown in a menu rather than in its row.
    #[must_use]
    pub fn test_part_hidden(&self, part: Part) -> bool {
        self.part_hidden(part)
    }

    /// The parts the shell shows to assistive technology and a check.
    #[must_use]
    pub fn test_exposed_parts(&self) -> Vec<Part> {
        self.exposed_parts()
    }

    /// How many of `column`'s actions stand in its header row.
    #[must_use]
    pub fn test_shown_actions(&self, column: Column) -> usize {
        self.shown_actions(column)
    }

    /// Whether the shell stands in for an empty list.
    #[must_use]
    pub fn test_nothing_listed_shown(&self) -> bool {
        self.nothing_listed_shown()
    }

    /// Lay the shell out again on the next pass.
    pub fn test_invalidate(&mut self) {
        self.invalidate();
    }

    /// Whether the content's change is still fading.
    #[must_use]
    pub fn test_content_fade_moving(&self, now: tilo_ui::time::Instant) -> bool {
        self.content_fade.is_moving(now)
    }

    /// How far the content's change has faded, from 0 to 1.
    #[must_use]
    pub fn test_content_fade_progress(&self, now: tilo_ui::time::Instant) -> f64 {
        self.content_fade.progress(now)
    }

    /// The key the content shows, if the view gave one.
    #[must_use]
    pub fn test_content_key(&self) -> Option<&str> {
        self.content_key.as_deref()
    }

    /// How far the column switch has faded, from 0 to 1.
    #[must_use]
    pub fn test_switch_fade_progress(&self, now: tilo_ui::time::Instant) -> f64 {
        self.switch_fade.progress(now)
    }

    /// Which columns the switch has turned on its way.
    #[must_use]
    pub fn test_switch_columns(&self) -> [bool; 4] {
        self.switch_columns
    }

    /// The parts the last pass laid out.
    #[must_use]
    pub fn test_laid_out_parts(&self) -> &[Part] {
        &self.laid_out_parts
    }

    /// Each column's header.
    #[must_use]
    pub fn test_headers(&self) -> &[Header; 4] {
        &self.headers
    }

    /// Each column's header, to choose an action from its menu.
    #[must_use]
    pub fn test_headers_mut(&mut self) -> &mut [Header; 4] {
        &mut self.headers
    }

    /// Whether the incoming page is still sliding in.
    #[must_use]
    pub fn test_push_moving(&self, now: tilo_ui::time::Instant) -> bool {
        self.inputs.borrow().push.is_moving(now)
    }

    /// How the sidebar adapts to its width, if it is a navigation list.
    #[must_use]
    pub fn test_adaptive(&self) -> Option<AdaptiveSidebar> {
        self.inputs.borrow().adaptive
    }
}

include!(concat!(env!("OUT_DIR"), "/component_registry.rs"));

/// The window shell, which a window builds by name beside the toolkit's
/// (`tilo_ui::component_catalogue::TOOLKIT`).
pub static COMPONENTS: tilo_ui_base::component::Set = tilo_ui_base::component::Set {
    entries: generated_entries,
    parts: GENERATED_PARTS,
    enums: Vec::new,
};
