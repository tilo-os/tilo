//! The Monitor window's widgets.
//!
//! The window is one [`ui::widget::NavShell`]. Its content is the active tab:
//! [`ProcessesPane`] (the summary tiles, the process filter and the process
//! table) or [`ResourcesPane`] (the system dashboard). The selected process
//! is the inspector ([`process_detail`]). [`crate::app::MonitorApp`] owns the
//! model and the shared view state; the panes meet it in a [`Bridge`]: the
//! app publishes what they show, the panes queue what the person asked for,
//! and the app turns each request into a view change or a document action.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use taffy::style_helpers::{length, percent};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::TextRole;
use ui::charts::{BarGauge, Chart, ChartStyle, Series, nice_step};
use ui::format::bytes as format_bytes;
use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::{TextKind, TextRenderer};
use ui::theme::Theme;
use ui::tilo_actions::{ActionAddress, ActionBinding, ControlInvocation, Resource};
use ui::widget::card::{CardGrid, CardWidth};
use ui::widget::data_table::{ColumnDef, ColumnWidth, DataTable};
use ui::widget::toolbar::Orientation;
use ui::widget::{
    AppModel, Button, ButtonRole, Column, Element, EmptyState, FilterCount, ModelPath, Property,
    PropertyList, Row, Scrollable, SearchField, Section, StatGroup, StatTile, TaffyTreeExt,
    Toolbar, Widget,
};

use crate::app::{SortKey, Tab};
use crate::model::{Metrics, ProcessDataStatus, ProcessInfo, fmt_rate, fmt_uptime};
use crate::tree::SumMode;

/// One row of the process table: what the list shows of a process.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ProcessRow {
    pub pid: u32,
    pub started_at: u64,
    pub name: String,
    pub user: String,
    pub cpu: f32,
    pub mem_bytes: u64,
    pub threads: usize,
    /// Depth in the process tree; 0 in a flat list.
    pub depth: usize,
    pub has_children: bool,
    pub expanded: bool,
}

/// Stable table identity for the exact process lifetime. The effect address still
/// carries both values and is checked against the live sampler at execution.
pub(crate) fn process_row_key(pid: u32, started_at: u64) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut key = std::collections::hash_map::DefaultHasher::new();
    (pid, started_at).hash(&mut key);
    key.finish()
}

/// A whole-system statistic shown as a tile above the process table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stat {
    Cpu,
    Memory,
    Network,
    Disk,
}

impl Stat {
    pub(crate) const ALL: [Self; 4] = [Self::Cpu, Self::Memory, Self::Network, Self::Disk];

    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Memory => "memory",
            Self::Network => "network",
            Self::Disk => "disk",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::Memory => "Memory",
            Self::Network => "Network",
            Self::Disk => "Disk",
        }
    }

    /// The dashboard section that shows this statistic in full.
    fn section_id(self) -> String {
        format!("monitor.section.{}", self.id())
    }
}

/// How often this window samples the machine, or whether it has paused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cadence {
    Every(u64),
    Paused,
}

/// The sampling cadences offered in the header, in milliseconds.
pub(crate) const INTERVALS: [u64; 5] = [1000, 1500, 2000, 3000, 5000];

impl Cadence {
    /// Every choice, in the order the header's menu lists them.
    pub(crate) fn all() -> Vec<Self> {
        INTERVALS
            .iter()
            .map(|ms| Self::Every(*ms))
            .chain([Self::Paused])
            .collect()
    }

    /// The choice in words, as the menu lists it.
    pub(crate) fn label(self) -> String {
        match self {
            Self::Paused => "Paused".to_owned(),
            Self::Every(1000) => "Every second".to_owned(),
            Self::Every(ms) => format!("Every {} seconds", seconds(ms)),
        }
    }

    /// The choice on the header's button.
    pub(crate) fn short_label(self) -> String {
        match self {
            Self::Paused => "Paused".to_owned(),
            Self::Every(ms) => format!("Every {} s", seconds(ms)),
        }
    }
}

/// `1500` → `1.5`, `2000` → `2`.
fn seconds(ms: u64) -> String {
    if ms.is_multiple_of(1000) {
        (ms / 1000).to_string()
    } else {
        format!("{:.1}", ms as f64 / 1000.0)
    }
}

/// What the person asked a pane for.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Request {
    Tab(Tab),
    Sort(SortKey),
    Filter(String),
    Select(u32),
    /// Enter or a double click on a row.
    Activate(u32),
    ToggleExpand(u32),
    SetExpanded(u32, bool),
    TreeMode(bool),
    SumChildren(bool),
    KernelThreads(bool),
    ConfirmKills(bool),
    /// The header's cadence button was pressed.
    ChooseCadence,
    ShowStat(Stat),
    ContextMenu {
        pid: u32,
        at: Point,
        /// The row the menu acts on, marked while the menu is open.
        row: Rect,
    },
    Retry,
}

/// What the process list shows in place of rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Placeholder {
    Loading,
    Empty,
    PermissionDenied,
    Unavailable,
    NoMatches,
}

impl Placeholder {
    pub(crate) fn of(status: ProcessDataStatus) -> Option<Self> {
        match status {
            ProcessDataStatus::Ready => None,
            ProcessDataStatus::Loading => Some(Self::Loading),
            ProcessDataStatus::Empty => Some(Self::Empty),
            ProcessDataStatus::PermissionDenied => Some(Self::PermissionDenied),
            ProcessDataStatus::Unavailable => Some(Self::Unavailable),
        }
    }
}

/// The table's display options.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ListOptions {
    pub sort: SortKey,
    pub descending: bool,
    pub tree: bool,
    pub sum: SumMode,
    pub kernel_threads: bool,
    pub confirm_kill: bool,
}

/// The state the app and its panes share.
#[derive(Default)]
pub(crate) struct Bridge {
    /// Bumped whenever anything below changes.
    pub revision: u64,
    /// Bumped when `rows` changes.
    pub rows_revision: u64,
    pub rows: Vec<ProcessRow>,
    /// The selected row, as an index into `rows`.
    pub selected: Option<usize>,
    pub options: Option<ListOptions>,
    pub filter: String,
    /// How many processes the list could show, filtered or not.
    pub total: usize,
    pub placeholder: Option<Placeholder>,
    /// Bumped when `metrics` changes.
    pub metrics_revision: u64,
    pub metrics: Option<Metrics>,
    pub process_count: usize,
    pub thread_count: usize,
    /// A dashboard section to bring into view.
    pub reveal: Option<Stat>,
    /// Open the row menu over this process once its row is laid out
    /// (captures).
    pub menu_seed: Option<u32>,
    requests: Vec<Request>,
}

impl Bridge {
    pub(crate) fn push(&mut self, request: Request) {
        self.requests.push(request);
    }

    pub(crate) fn take_requests(&mut self) -> Vec<Request> {
        std::mem::take(&mut self.requests)
    }
}

pub(crate) type Shared = Rc<RefCell<Bridge>>;

/// A value and its unit, split for a stat tile: `8.6 GiB` → `8.6`, `GiB`.
fn value_and_unit(text: &str) -> (String, String) {
    match text.rsplit_once(' ') {
        Some((value, unit)) => (value.to_owned(), unit.to_owned()),
        None => (text.to_owned(), String::new()),
    }
}

fn percent_label(value: f32) -> String {
    format!("{value:.0}%")
}

/// The four whole-system tiles over the process table, each with its recent
/// history. Pressing one opens its section of the dashboard.
fn summary_tiles(metrics: &Metrics, shared: &Shared) -> StatGroup {
    let tiles = Stat::ALL
        .iter()
        .map(|&stat| {
            let (value, unit, samples) = match stat {
                Stat::Cpu => (
                    format!("{:.0}", metrics.cpu_overall()),
                    "%".to_owned(),
                    metrics.cpu_hist.as_slice().to_vec(),
                ),
                Stat::Memory => {
                    let (value, unit) = value_and_unit(&format_bytes(metrics.mem_used));
                    (value, unit, metrics.mem_hist.as_slice().to_vec())
                }
                Stat::Network => {
                    let (value, unit) = value_and_unit(&fmt_rate(metrics.net_rx + metrics.net_tx));
                    let samples = metrics
                        .net_rx_hist
                        .as_slice()
                        .iter()
                        .zip(metrics.net_tx_hist.as_slice())
                        .map(|(rx, tx)| rx + tx)
                        .collect();
                    (value, unit, samples)
                }
                Stat::Disk => {
                    let (value, unit) =
                        value_and_unit(&fmt_rate(metrics.disk_read + metrics.disk_write));
                    let samples = metrics
                        .disk_read_hist
                        .as_slice()
                        .iter()
                        .zip(metrics.disk_write_hist.as_slice())
                        .map(|(read, write)| read + write)
                        .collect();
                    (value, unit, samples)
                }
            };
            let requests = Rc::clone(shared);
            let mut tile = StatTile::new(stat.label(), value)
                .sparkline(samples)
                .on_activate(format!("monitor.summary.{}", stat.id()), move || {
                    requests.borrow_mut().push(Request::ShowStat(stat));
                });
            if !unit.is_empty() {
                tile = tile.unit(unit);
            }
            Box::new(tile) as Element
        })
        .collect();
    StatGroup::new(tiles)
}

/// The chevron hit areas the name column painted this frame, by pid.
#[derive(Clone, Default)]
struct Twisties(Arc<Mutex<Vec<(u32, Rect)>>>);

impl Twisties {
    fn clear(&self) {
        if let Ok(mut rects) = self.0.lock() {
            rects.clear();
        }
    }

    fn record(&self, pid: u32, rect: Rect) {
        if let Ok(mut rects) = self.0.lock() {
            rects.push((pid, rect));
        }
    }

    fn at(&self, point: Point) -> Option<u32> {
        self.0
            .lock()
            .ok()?
            .iter()
            .find(|(_, rect)| rect.contains(point))
            .map(|(pid, _)| *pid)
    }
}

/// Paint a name cell: indented by depth in a tree, with a disclosure
/// chevron on a parent, then the name elided to the cell.
fn paint_name(
    row: &ProcessRow,
    tree: bool,
    twisties: &Twisties,
    scene: &mut Scene,
    theme: &Theme,
    text: &mut TextRenderer,
    cell: Rect,
) {
    let tokens = &theme.tokens;
    let pad = f64::from(tokens.spacing.s2);
    let style = theme.text(TextRole::Caption);
    let mut x = cell.x0 + pad;
    if tree {
        x += f64::from(tokens.spacing.s4) * row.depth as f64;
        let icon = f64::from(tokens.metrics.icon_sm);
        if row.has_children {
            let glyph =
                Rect::from_center_size(Point::new(x + icon / 2.0, cell.center().y), (icon, icon));
            let chevron = if row.expanded {
                Icon::ChevronDown
            } else {
                Icon::ChevronRight
            };
            chevron.draw(scene, Affine::IDENTITY, glyph, theme.text_dim);
            let reach = f64::from(tokens.spacing.s1);
            twisties.record(
                row.pid,
                Rect::new(glyph.x0 - reach, cell.y0, glyph.x1 + reach, cell.y1),
            );
        }
        x += icon + f64::from(tokens.spacing.s1);
    }
    let room = (cell.x1 - pad - x).max(0.0);
    let label = text.elide_to_width_style(&row.name, room as f32, &style);
    let (_, h) = text.measure_style(&label, None, &style);
    let y = ui::text_metrics::centered_text_top(cell.y0, cell.height(), f64::from(h)).max(cell.y0);
    text.draw_style(
        scene,
        &label,
        theme.text_primary,
        Affine::translate((x, y)),
        None,
        &style,
    );
}

/// The table's shape: it is rebuilt when one of these changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TableShape {
    tree: bool,
    /// A narrow window drops the thread and PID columns.
    compact: bool,
}

/// The process table for `shape`, and the sort key of each of its columns.
fn process_table(
    shape: TableShape,
    twisties: &Twisties,
    shared: &Shared,
) -> (DataTable<ProcessRow>, Vec<SortKey>) {
    let tree = shape.tree;
    let painted = twisties.clone();
    let mut columns = vec![(
        SortKey::Name,
        ColumnDef::new("name", "Name", ColumnWidth::Flex(1.0))
            .with_render(Box::new(
                move |row: &ProcessRow, scene, theme, text, cell| {
                    paint_name(row, tree, &painted, scene, theme, text, cell);
                },
            ))
            .with_accessible_value(|row: &ProcessRow| row.name.clone()),
    )];
    columns.push((
        SortKey::Cpu,
        ColumnDef::new("cpu", "CPU", ColumnWidth::Auto)
            .with_kind(TextKind::Numeric)
            .with_text(|row: &ProcessRow| format!("{:.1}%", row.cpu)),
    ));
    columns.push((
        SortKey::Mem,
        ColumnDef::new("memory", "Memory", ColumnWidth::Auto)
            .with_kind(TextKind::Numeric)
            .with_text(|row: &ProcessRow| format_bytes(row.mem_bytes)),
    ));
    if !shape.compact {
        columns.push((
            SortKey::Threads,
            ColumnDef::new("threads", "Threads", ColumnWidth::Auto)
                .with_kind(TextKind::Numeric)
                .with_text(|row: &ProcessRow| row.threads.to_string()),
        ));
        columns.push((
            SortKey::Pid,
            ColumnDef::new("pid", "PID", ColumnWidth::Auto)
                .with_kind(TextKind::Numeric)
                .with_text(|row: &ProcessRow| row.pid.to_string()),
        ));
    }
    columns.push((
        SortKey::User,
        ColumnDef::new("user", "User", ColumnWidth::Auto)
            .with_kind(TextKind::Identifier)
            .with_text(|row: &ProcessRow| row.user.clone()),
    ));
    let keys: Vec<SortKey> = columns.iter().map(|(key, _)| *key).collect();
    let columns = columns
        .into_iter()
        .map(|(_, column)| column.sortable(true))
        .collect();

    let header_keys = keys.clone();
    let sorts = Rc::clone(shared);
    let activations = Rc::clone(shared);
    let table = DataTable::new(columns)
        .with_id("monitor.processes")
        .with_accessible_name("Processes")
        .selectable(true)
        .with_row_key(|row: &ProcessRow| process_row_key(row.pid, row.started_at))
        .on_header_click(move |index| {
            if let Some(key) = header_keys.get(index) {
                sorts.borrow_mut().push(Request::Sort(*key));
            }
        })
        .on_activate(move |index| {
            let pid = activations.borrow().rows.get(index).map(|row| row.pid);
            if let Some(pid) = pid {
                activations.borrow_mut().push(Request::Activate(pid));
            }
        });
    (table, keys)
}

/// The column key the table sorts by, for its header indicator.
fn column_key(sort: SortKey) -> &'static str {
    match sort {
        SortKey::Pid => "pid",
        SortKey::Name => "name",
        SortKey::User => "user",
        SortKey::Cpu => "cpu",
        SortKey::Mem => "memory",
        SortKey::Threads => "threads",
    }
}

/// The view toggles beside the filter.
fn view_toolbar(options: ListOptions, shared: &Shared) -> Toolbar {
    let tree = Rc::clone(shared);
    let sum = Rc::clone(shared);
    let kernel = Rc::clone(shared);
    let confirm = Rc::clone(shared);
    let mut toolbar =
        Toolbar::new(Orientation::Horizontal).push_toggle("Tree view", options.tree, move || {
            tree.borrow_mut().push(Request::TreeMode(!options.tree));
        });
    if options.tree {
        let summing = options.sum == SumMode::SumSubtree;
        toolbar = toolbar.push_toggle("Include children", summing, move || {
            sum.borrow_mut().push(Request::SumChildren(!summing));
        });
    }
    toolbar
        .push_separator()
        .push_toggle("Kernel threads", options.kernel_threads, move || {
            kernel
                .borrow_mut()
                .push(Request::KernelThreads(!options.kernel_threads));
        })
        .push_toggle("Confirm kills", options.confirm_kill, move || {
            confirm
                .borrow_mut()
                .push(Request::ConfirmKills(!options.confirm_kill));
        })
}

/// One sentence and at most one action for each way the list is empty.
fn empty_state(kind: Placeholder) -> EmptyState {
    match kind {
        Placeholder::Loading => EmptyState::new("Reading processes"),
        Placeholder::Empty => EmptyState::new("No processes are running"),
        Placeholder::PermissionDenied => EmptyState::error(
            "Processes unavailable",
            "This account isn't allowed to read the process list.",
        )
        .with_action("Try again"),
        Placeholder::Unavailable => EmptyState::error(
            "Processes unavailable",
            "The process list couldn't be read.",
        )
        .with_action("Try again"),
        Placeholder::NoMatches => {
            EmptyState::new("No processes match the filter").with_action("Clear filter")
        }
    }
}

/// `node` grown or shrunk along a band's main axis.
fn flexed(taffy: &mut TaffyTree<()>, node: NodeId, grow: f32) {
    if let Ok(mut style) = taffy.style(node).cloned() {
        style.flex_grow = grow;
        style.flex_shrink = 1.0;
        style.flex_basis = length(0.0_f32);
        style.size.width = Dimension::auto();
        let _ = taffy.set_style(node, style);
    }
}

fn origin_of(taffy: &TaffyTree<()>, node: NodeId, parent: Point) -> Point {
    taffy.layout(node).map_or(parent, |layout| {
        Point::new(
            parent.x + f64::from(layout.location.x),
            parent.y + f64::from(layout.location.y),
        )
    })
}

fn rect_of(taffy: &TaffyTree<()>, node: NodeId, origin: Point) -> Rect {
    taffy.layout(node).map_or(Rect::ZERO, |layout| {
        Rect::from_origin_size(
            (
                origin.x + f64::from(layout.location.x),
                origin.y + f64::from(layout.location.y),
            ),
            (f64::from(layout.size.width), f64::from(layout.size.height)),
        )
    })
}

/// Child node ids of the last layout.
#[derive(Default)]
struct Nodes {
    summary: Option<NodeId>,
    band: Option<NodeId>,
    field: Option<NodeId>,
    toolbar: Option<NodeId>,
    body: Option<NodeId>,
    body_child: Option<NodeId>,
}

/// The Processes tab: the summary tiles, the filter with the view toggles,
/// and the process table, or one empty state when there is nothing to list.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent layout, structure and selection-reveal flags"
)]
pub(crate) struct ProcessesPane {
    shared: Shared,
    seen: Option<u64>,
    seen_rows: Option<u64>,
    seen_metrics: Option<u64>,
    summary: Option<StatGroup>,
    /// The metrics changed since `summary` was built.
    summary_stale: bool,
    field: SearchField,
    toolbar: Toolbar,
    options: Option<ListOptions>,
    table: DataTable<ProcessRow>,
    sort_keys: Vec<SortKey>,
    shape: TableShape,
    twisties: Twisties,
    /// The viewport height last handed to the table.
    viewport: f64,
    row_height: f64,
    placeholder: Option<(Placeholder, EmptyState)>,
    has_processes: bool,
    /// The row index the table's selection was last synchronised to.
    shown_selection: Option<usize>,
    adopted_selection: Option<usize>,
    reveal_selection: bool,
    cursor: Point,
    rect: Rect,
    /// Below this width the pane is compact (from the last layout's theme).
    compact_below: f64,
    /// How far the table insets a row's selection lift (from the last
    /// layout's theme).
    lift_inset: f64,
    nodes: Nodes,
    layout_dirty: bool,
    structure_dirty: bool,
}

impl ProcessesPane {
    pub(crate) fn new(shared: Shared) -> Self {
        let twisties = Twisties::default();
        let shape = TableShape {
            tree: true,
            compact: false,
        };
        let (table, sort_keys) = process_table(shape, &twisties, &shared);
        Self {
            seen: None,
            seen_rows: None,
            seen_metrics: None,
            summary: None,
            summary_stale: true,
            field: SearchField::filter()
                .with_id("monitor.filter")
                .with_placeholder("Filter processes"),
            toolbar: Toolbar::new(Orientation::Horizontal),
            options: None,
            table,
            sort_keys,
            shape,
            twisties,
            viewport: 0.0,
            row_height: 0.0,
            placeholder: None,
            has_processes: false,
            shown_selection: None,
            adopted_selection: None,
            reveal_selection: false,
            cursor: Point::ZERO,
            rect: Rect::ZERO,
            compact_below: 0.0,
            lift_inset: 0.0,
            nodes: Nodes::default(),
            layout_dirty: true,
            structure_dirty: true,
            shared,
        }
    }

    /// Adopt what the app published since the last look.
    fn adopt(&mut self) {
        let shared = Rc::clone(&self.shared);
        let bridge = shared.borrow();
        if self.seen == Some(bridge.revision) {
            return;
        }
        self.seen = Some(bridge.revision);

        if self.seen_metrics != Some(bridge.metrics_revision) {
            self.seen_metrics = Some(bridge.metrics_revision);
            self.summary_stale = true;
            self.structure_dirty = true;
        }

        if let Some(options) = bridge.options
            && self.options != Some(options)
        {
            let tree_changed = self.options.is_none_or(|old| old.tree != options.tree);
            self.options = Some(options);
            self.toolbar = view_toolbar(options, &self.shared);
            if tree_changed {
                self.shape.tree = options.tree;
                self.rebuild_table();
            }
            self.show_sort(options);
            self.structure_dirty = true;
        }

        if self.seen_rows != Some(bridge.rows_revision) {
            self.seen_rows = Some(bridge.rows_revision);
            self.table.set_rows(bridge.rows.clone());
            self.shown_selection = None;
        }
        self.show_selection(bridge.selected);
        if bridge.selected != self.adopted_selection {
            self.adopted_selection = bridge.selected;
            self.reveal_selection = bridge.selected.is_some();
        }

        if self.field.value() != bridge.filter {
            self.field.set_query(&bridge.filter);
        }
        self.field.set_count(
            (!bridge.filter.is_empty()).then(|| FilterCount::new(bridge.rows.len(), bridge.total)),
        );

        let placeholder = bridge.placeholder;
        if self.placeholder.as_ref().map(|(kind, _)| *kind) != placeholder {
            self.placeholder = placeholder.map(|kind| (kind, empty_state(kind)));
            self.structure_dirty = true;
        }
        let has_processes = bridge.total > 0;
        if self.has_processes != has_processes {
            self.has_processes = has_processes;
            self.structure_dirty = true;
        }
        self.layout_dirty = true;
    }

    fn show_sort(&mut self, options: ListOptions) {
        // The app orders the rows (a tree sorts each group of siblings);
        // the table keeps that order and shows which column it is by.
        self.table.sort_by_with(
            |_, _| std::cmp::Ordering::Equal,
            column_key(options.sort),
            !options.descending,
        );
    }

    fn rebuild_table(&mut self) {
        let rows = self.table.view_rows().into_iter().cloned().collect();
        let (table, keys) = process_table(self.shape, &self.twisties, &self.shared);
        self.table = table;
        self.sort_keys = keys;
        self.table.set_rows(rows);
        if let Some(options) = self.options {
            self.show_sort(options);
        }
        self.shown_selection = None;
        let selected = self.shared.borrow().selected;
        self.show_selection(selected);
        self.viewport = 0.0;
        self.structure_dirty = true;
    }

    fn show_selection(&mut self, selected: Option<usize>) {
        if self.shown_selection == selected && selected.is_some() {
            return;
        }
        let selection = self.table.selection_mut();
        match selected {
            Some(index) => selection.select_only(index),
            None => selection.clear(),
        }
        self.shown_selection = selected;
    }

    /// Queue the row the table's own selection moved to (a click, the
    /// arrow keys) as the shared selection.
    fn queue_selection(&mut self) -> Option<u32> {
        let primary = self.table.selection().primary();
        let pid = primary.and_then(|index| self.table.view_rows().get(index).map(|row| row.pid));
        if primary == self.shown_selection {
            return pid;
        }
        self.shown_selection = primary;
        if let Some(pid) = pid {
            self.shared.borrow_mut().push(Request::Select(pid));
        }
        pid
    }

    /// Scroll the table so the selected row is in view, centred when it was
    /// out of sight.
    fn scroll_to_selection(&mut self) {
        let Some(index) = self.table.selection().primary() else {
            return;
        };
        let top = index as f64 * self.row_height;
        let scroll = f64::from(self.table.scroll_y());
        if top < scroll || top + self.row_height > scroll + self.viewport {
            let centred = (top - (self.viewport - self.row_height) / 2.0).max(0.0);
            self.table.set_scroll_y(centred as f32);
            self.layout_dirty = true;
        }
    }

    /// `pid`'s row as its selection lift covers it, once the rows are laid
    /// out.
    fn row_rect(&self, pid: u32) -> Option<Rect> {
        let table = self.table.get_rect();
        if table.width() <= 0.0 || self.viewport <= 0.0 {
            return None;
        }
        let index = self
            .table
            .view_rows()
            .iter()
            .position(|row| row.pid == pid)?;
        let header = (table.height() - self.viewport).max(0.0);
        let y0 =
            table.y0 + header + index as f64 * self.row_height - f64::from(self.table.scroll_y());
        Some(Rect::new(
            table.x0 + self.lift_inset,
            y0,
            table.x1 - self.lift_inset,
            y0 + self.row_height,
        ))
    }

    /// Where a person right-clicks `pid`'s row: in its name cell, past the
    /// name, so the menu leaves the name readable.
    fn row_point(&self, pid: u32) -> Option<(Point, Rect)> {
        let row = self.row_rect(pid)?;
        let name = self
            .table
            .column_width_px("name")
            .map_or_else(|| row.width() / 3.0, f64::from);
        Some((Point::new(row.x0 + name * 0.6, row.center().y), row))
    }

    /// The process whose row is under `point`, read from the rows the table
    /// publishes (their bounds and their `row:<pid>:<n>` targets).
    fn row_pid_at(&self, point: Point) -> Option<u32> {
        let table = self.table.accessibility()?;
        let row = table.children.iter().find(|row| {
            row.bounds
                .is_some_and(|bounds| bounds.to_rect().contains(point))
        })?;
        let key: u64 = row
            .action_target
            .as_deref()?
            .strip_prefix("row:")?
            .split(':')
            .next()?
            .parse()
            .ok()?;
        self.shared
            .borrow()
            .rows
            .iter()
            .find(|row| process_row_key(row.pid, row.started_at) == key)
            .map(|row| row.pid)
    }

    /// Open a seeded row menu once the rows are laid out.
    fn open_seeded_menu(&self) {
        let Some(pid) = self.shared.borrow().menu_seed else {
            return;
        };
        let Some((at, row)) = self.row_point(pid) else {
            return;
        };
        let mut bridge = self.shared.borrow_mut();
        bridge.menu_seed = None;
        bridge.push(Request::ContextMenu { pid, at, row });
    }

    fn selected_row(&self) -> Option<ProcessRow> {
        let index = self.table.selection().primary()?;
        self.table.view_rows().get(index).map(|row| (*row).clone())
    }

    /// A pane `width` wide drops columns and stacks its filter.
    fn is_compact(&self, width: f64) -> bool {
        width > 0.0 && width < self.compact_below
    }

    fn shows_filters(&self) -> bool {
        self.has_processes
    }

    fn body(&self) -> &dyn Widget {
        match &self.placeholder {
            Some((_, empty)) => empty,
            None => &self.table,
        }
    }

    fn body_mut(&mut self) -> &mut dyn Widget {
        match &mut self.placeholder {
            Some((_, empty)) => empty,
            None => &mut self.table,
        }
    }

    /// The filter beside the view toggles, or over them on a narrow pane.
    fn build_filters(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        compact: bool,
    ) -> NodeId {
        let rhythm = &theme.tokens.spacing.rhythm;
        let field = self.field.build_layout(taffy, theme, node_map);
        let toolbar = self.toolbar.build_layout(taffy, theme, node_map);
        let (direction, align) = if compact {
            (taffy::FlexDirection::Column, taffy::AlignItems::Stretch)
        } else {
            flexed(taffy, field, 1.0);
            (taffy::FlexDirection::Row, taffy::AlignItems::Center)
        };
        let band = taffy.parent_node(
            Style {
                display: taffy::Display::Flex,
                flex_direction: direction,
                flex_shrink: 0.0,
                align_items: Some(align),
                gap: Size {
                    width: length(rhythm.sibling),
                    height: length(rhythm.related),
                },
                size: Size {
                    width: percent(1.0_f32),
                    height: Dimension::auto(),
                },
                ..Default::default()
            },
            &[field, toolbar],
        );
        self.nodes.band = Some(band);
        self.nodes.field = Some(field);
        self.nodes.toolbar = Some(toolbar);
        band
    }

    /// Route an event to the table, with the gestures the table leaves to
    /// its host: the disclosure chevrons, ← and → on a parent, and the row
    /// menu.
    fn route_to_table(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
        child: NodeId,
    ) {
        let tree = self.options.is_some_and(|options| options.tree);
        match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if tree => {
                if let Some(pid) = self.twisties.at(self.cursor) {
                    self.shared.borrow_mut().push(Request::ToggleExpand(pid));
                    *ctx.handled = true;
                    return;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } if self.table.get_rect().contains(self.cursor) => {
                // Choose the row under the pointer, then offer its actions.
                if let Some(pid) = self.row_pid_at(self.cursor)
                    && let Some(index) =
                        self.table.view_rows().iter().position(|row| row.pid == pid)
                {
                    self.table.selection_mut().select_only(index);
                    self.queue_selection();
                    let row = self.row_rect(pid).unwrap_or_else(|| {
                        Rect::from_center_size(self.cursor, (0.0, self.row_height))
                    });
                    self.shared.borrow_mut().push(Request::ContextMenu {
                        pid,
                        at: self.cursor,
                        row,
                    });
                }
                *ctx.handled = true;
                return;
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(key @ (NamedKey::ArrowLeft | NamedKey::ArrowRight)),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } if tree && self.table.is_focused() => {
                if let Some(row) = self.selected_row()
                    && row.has_children
                {
                    let open = *key == NamedKey::ArrowRight;
                    if row.expanded != open {
                        self.shared
                            .borrow_mut()
                            .push(Request::SetExpanded(row.pid, open));
                    }
                    *ctx.handled = true;
                    return;
                }
            }
            _ => {}
        }
        self.table
            .handle_event(event, &mut ctx.reborrow(ctx.path, child));
        self.queue_selection();
    }
}

impl Widget for ProcessesPane {
    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        self.adopt();
        for child in self.children_mut() {
            child.sync(state, path);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.adopt();
        let tokens = &theme.tokens;
        let metrics = tokens.metrics;
        self.compact_below = f64::from(metrics.content_min_width + metrics.detail_min_width);
        self.lift_inset = f64::from(tokens.spacing.s2);
        let compact = self.is_compact(self.rect.width());
        if compact != self.shape.compact {
            self.shape.compact = compact;
            self.rebuild_table();
        }
        if self.summary_stale {
            self.summary_stale = false;
            self.summary = self
                .shared
                .borrow()
                .metrics
                .as_ref()
                .map(|metrics| summary_tiles(metrics, &self.shared));
        }
        self.row_height = f64::from(metrics.min_pointer_target);
        self.layout_dirty = false;
        self.nodes = Nodes::default();

        let gutter = tokens.spacing.rhythm.group;
        let gap = tokens.spacing.rhythm.section;
        let mut children = Vec::new();
        if let Some(summary) = &mut self.summary {
            let node = summary.build_layout(taffy, theme, node_map);
            self.nodes.summary = Some(node);
            children.push(node);
        }
        if self.shows_filters() {
            children.push(self.build_filters(taffy, theme, node_map, compact));
        }

        let body_child = self.body_mut().build_layout(taffy, theme, node_map);
        let body = taffy.parent_node(
            Style {
                display: taffy::Display::Flex,
                flex_direction: taffy::FlexDirection::Column,
                flex_grow: 1.0,
                flex_shrink: 1.0,
                min_size: Size {
                    width: Dimension::auto(),
                    height: length(0.0_f32),
                },
                size: Size {
                    width: percent(1.0_f32),
                    height: Dimension::auto(),
                },
                overflow: taffy::Point {
                    x: taffy::Overflow::Clip,
                    y: taffy::Overflow::Clip,
                },
                ..Default::default()
            },
            &[body_child],
        );
        self.nodes.body = Some(body);
        self.nodes.body_child = Some(body_child);
        children.push(body);

        let node = taffy.parent_node(
            Style {
                display: taffy::Display::Flex,
                flex_direction: taffy::FlexDirection::Column,
                flex_grow: 1.0,
                gap: Size {
                    width: length(0.0_f32),
                    height: length(gap),
                },
                padding: taffy::Rect {
                    left: length(gutter),
                    right: length(gutter),
                    top: length(tokens.spacing.rhythm.sibling),
                    bottom: length(0.0_f32),
                },
                size: Size {
                    width: percent(1.0_f32),
                    height: percent(1.0_f32),
                },
                ..Default::default()
            },
            &children,
        );
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        self.structure_dirty = false;
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        let x0 = origin.x + f64::from(l.location.x);
        let y0 = origin.y + f64::from(l.location.y);
        let width_changed = (f64::from(l.size.width) - self.rect.width()).abs() > 0.5;
        self.rect = Rect::new(
            x0,
            y0,
            x0 + f64::from(l.size.width),
            y0 + f64::from(l.size.height),
        );
        let inner = Point::new(x0, y0);
        if let (Some(summary), Some(node)) = (&mut self.summary, self.nodes.summary) {
            summary.apply_layout(taffy, node, inner);
        }
        if let Some(band) = self.nodes.band {
            let band_origin = origin_of(taffy, band, inner);
            if let Some(field) = self.nodes.field {
                self.field.apply_layout(taffy, field, band_origin);
            }
            if let Some(toolbar) = self.nodes.toolbar {
                self.toolbar.apply_layout(taffy, toolbar, band_origin);
            }
        }
        if let (Some(body), Some(child)) = (self.nodes.body, self.nodes.body_child) {
            let body_origin = origin_of(taffy, body, inner);
            let available = rect_of(taffy, body, inner).height();
            self.body_mut().apply_layout(taffy, child, body_origin);
            if self.placeholder.is_none() {
                // The table is as tall as its header plus the viewport it
                // was given; hand it the viewport that fills the body.
                let header = (self.table.get_rect().height() - self.viewport).max(0.0);
                let target = if self.viewport > 0.0 {
                    (available - header).max(self.row_height)
                } else {
                    self.row_height
                };
                if (target - self.viewport).abs() > 0.5 {
                    self.table
                        .set_viewport(target as f32, self.row_height as f32);
                    self.viewport = target;
                    self.layout_dirty = true;
                } else if self.reveal_selection {
                    self.reveal_selection = false;
                    self.scroll_to_selection();
                } else {
                    self.open_seeded_menu();
                }
            }
        }
        if width_changed {
            self.layout_dirty = true;
            if self.is_compact(self.rect.width()) != self.shape.compact {
                self.structure_dirty = true;
            }
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.twisties.clear();
        if let Some(summary) = &mut self.summary {
            summary.draw(scene, transform, text_renderer, theme, presence);
        }
        if self.shows_filters() {
            self.field
                .draw(scene, transform, text_renderer, theme, presence);
            self.toolbar
                .draw(scene, transform, text_renderer, theme, presence);
        }
        self.body_mut()
            .draw(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
            // Every part follows the pointer, whichever one is under it.
            let mut ignored = false;
            let nodes = [
                self.nodes.toolbar,
                self.nodes.field,
                self.nodes.summary,
                self.nodes.body_child,
            ];
            let shows_filters = self.shows_filters();
            let summary = self.summary.as_mut().map(|s| s as &mut dyn Widget);
            let body: &mut dyn Widget = match &mut self.placeholder {
                Some((_, empty)) => empty,
                None => &mut self.table,
            };
            let mut parts: Vec<(Option<NodeId>, &mut dyn Widget)> = Vec::new();
            if shows_filters {
                parts.push((nodes[0], &mut self.toolbar));
                parts.push((nodes[1], &mut self.field));
            }
            if let Some(summary) = summary {
                parts.push((nodes[2], summary));
            }
            parts.push((nodes[3], body));
            for (node, part) in parts {
                if let Some(node) = node {
                    let mut child = ctx.reborrow(ctx.path, node);
                    child.handled = &mut ignored;
                    part.handle_event(event, &mut child);
                }
            }
            return;
        }
        if self.shows_filters() {
            if let Some(toolbar) = self.nodes.toolbar {
                self.toolbar
                    .handle_event(event, &mut ctx.reborrow(ctx.path, toolbar));
                if *ctx.handled {
                    return;
                }
            }
            if let Some(field) = self.nodes.field {
                self.field
                    .handle_event(event, &mut ctx.reborrow(ctx.path, field));
                if let Some(query) = self.field.take_dirty() {
                    self.shared.borrow_mut().push(Request::Filter(query));
                }
                if *ctx.handled {
                    return;
                }
            }
        }
        if let (Some(summary), Some(node)) = (&mut self.summary, self.nodes.summary) {
            summary.handle_event(event, &mut ctx.reborrow(ctx.path, node));
            if *ctx.handled {
                return;
            }
        }
        let Some(child) = self.nodes.body_child else {
            return;
        };
        if let Some((kind, empty)) = &mut self.placeholder {
            empty.handle_event(event, &mut ctx.reborrow(ctx.path, child));
            if empty.take_action() {
                let request = match kind {
                    Placeholder::NoMatches => Request::Filter(String::new()),
                    _ => Request::Retry,
                };
                self.shared.borrow_mut().push(request);
            }
        } else {
            self.route_to_table(event, ctx, child);
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        let mut children: Vec<&dyn Widget> = Vec::new();
        if let Some(summary) = &self.summary {
            children.push(summary);
        }
        if self.shows_filters() {
            children.push(&self.field);
            children.push(&self.toolbar);
        }
        children.push(self.body());
        children
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        for child in self.children() {
            visitor(child);
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let shows_filters = self.shows_filters();
        let body: &mut dyn Widget = match &mut self.placeholder {
            Some((_, empty)) => empty,
            None => &mut self.table,
        };
        let mut children: Vec<&mut (dyn Widget + '_)> = Vec::new();
        if let Some(summary) = &mut self.summary {
            children.push(summary);
        }
        if shows_filters {
            children.push(&mut self.field);
            children.push(&mut self.toolbar);
        }
        children.push(body);
        children
    }

    fn wants_pointer_capture(&self) -> bool {
        self.children().iter().any(|c| c.wants_pointer_capture())
    }

    fn update(&mut self, state: &mut AppModel, path: &ModelPath) -> bool {
        let mut changed = false;
        for child in self.children_mut() {
            changed |= child.update(state, path);
        }
        changed
    }

    fn is_layout_dirty(&self) -> bool {
        self.layout_dirty || self.children().iter().any(|c| c.is_layout_dirty())
    }

    fn layout_needs_measure(&self) -> bool {
        self.layout_dirty || self.children().iter().any(|c| c.layout_needs_measure())
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mut dirty = std::mem::take(&mut self.structure_dirty);
        for child in self.children_mut() {
            dirty |= child.take_structure_dirty();
        }
        dirty
    }

    fn name(&self) -> &'static str {
        "ProcessesPane"
    }

    fn test_id(&self) -> Option<&str> {
        Some("monitor.processes-pane")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

/// Horizontal grid divisions of a history chart.
const GRID_DIVISIONS: usize = 4;

/// A chart of `series`, scaled to `max`, with its high and low axis labels.
fn history_chart(series: Vec<Series>, max: f32, high: &str) -> Chart {
    Chart::new(series)
        .with_area(true)
        .with_style(ChartStyle::default().with_grid(GRID_DIVISIONS, None))
        .with_max(max)
        .with_axis_labels(high, "0")
}

/// The top of a throughput chart's scale for a `peak` rate: a round number,
/// one grid step at a round size above the highest grid line the peak
/// reaches, so the series stays out of the top band that holds the scale's
/// label.
fn axis_top(peak: f32) -> f32 {
    let below_top = (GRID_DIVISIONS - 1) as f64;
    let peak = f64::from(peak.max(1.0));
    let mut step = nice_step(peak / below_top);
    // A 1-2-5 step one size up more than covers a shortfall, so this ends
    // after one round.
    for _ in 0..2 {
        if step * below_top >= peak {
            break;
        }
        step = nice_step(step * 1.6);
    }
    (step * GRID_DIVISIONS as f64) as f32
}

/// A level gauge: `label` over a bar filled to `frac`, with its reading.
fn level_gauge(label: impl Into<String>, reading: String, frac: f32, theme: &Theme) -> Element {
    Box::new(
        BarGauge::new(frac)
            .with_label(label, reading)
            .with_color(theme.tokens.colors.intent.inverted),
    )
}

/// The processor: the load history and a gauge per core.
fn processor_section(metrics: &Metrics, theme: &Theme) -> Section {
    let cpu_chart = history_chart(
        vec![
            Series::new(
                metrics.cpu_hist.as_slice().to_vec(),
                theme.tokens.colors.presence.series(0),
            )
            .with_legend(format!("Total {}", percent_label(metrics.cpu_overall()))),
        ],
        100.0,
        "100%",
    );
    let cores: Vec<Element> = metrics
        .cores
        .iter()
        .enumerate()
        .map(|(index, usage)| {
            level_gauge(
                format!("Core {}", index + 1),
                percent_label(*usage),
                usage / 100.0,
                theme,
            )
        })
        .collect();
    Section::titled(
        "Processor",
        vec![
            Box::new(cpu_chart),
            Box::new(CardGrid::new(cores).with_card_width(CardWidth::Compact)),
        ],
    )
}

/// Memory and swap in use, and the memory history.
fn memory_section(metrics: &Metrics, theme: &Theme) -> Section {
    let of = |used: u64, total: u64| format!("{} of {}", format_bytes(used), format_bytes(total));
    let history: Vec<f32> = metrics
        .mem_hist
        .as_slice()
        .iter()
        .map(|frac| frac * 100.0)
        .collect();
    Section::titled(
        "Memory",
        vec![
            level_gauge(
                "RAM",
                of(metrics.mem_used, metrics.mem_total),
                metrics.mem_frac(),
                theme,
            ),
            level_gauge(
                "Swap",
                of(metrics.swap_used, metrics.swap_total),
                metrics.swap_frac(),
                theme,
            ),
            Box::new(history_chart(
                vec![
                    Series::new(history, theme.tokens.colors.presence.series(1)).with_legend(
                        format!("In use {}", percent_label(metrics.mem_frac() * 100.0)),
                    ),
                ],
                100.0,
                "100%",
            )),
        ],
    )
}

/// One direction of a throughput chart: its history, current rate and name.
struct Flow<'a> {
    history: &'a [f32],
    rate: f32,
    name: &'a str,
}

/// A section charting two flows (received and sent, read and written) on
/// one scale.
fn throughput_section(
    title: &str,
    flows: &[Flow<'_>; 2],
    first_series: usize,
    theme: &Theme,
) -> Section {
    let peak = flows
        .iter()
        .flat_map(|flow| flow.history.iter().copied())
        .fold(1.0_f32, f32::max);
    let max = axis_top(peak);
    let series = flows
        .iter()
        .enumerate()
        .map(|(index, flow)| {
            Series::new(
                flow.history.to_vec(),
                theme.tokens.colors.presence.series(first_series + index),
            )
            .with_legend(format!("{} {}", flow.name, fmt_rate(flow.rate)))
        })
        .collect();
    Section::titled(
        title,
        vec![Box::new(history_chart(series, max, &fmt_rate(max)))],
    )
}

/// The machine's facts.
fn system_section(metrics: &Metrics, process_count: usize, thread_count: usize) -> Section {
    let numeric =
        |label: &str, value: String| Property::new(label, value).with_kind(TextKind::Numeric);
    let load = format!(
        "{:.2}, {:.2}, {:.2}",
        metrics.load_avg[0], metrics.load_avg[1], metrics.load_avg[2]
    );
    Section::titled(
        "System",
        vec![Box::new(PropertyList::new(vec![
            Property::new("Host name", metrics.host.clone()).with_kind(TextKind::Identifier),
            Property::new("Kernel", metrics.kernel.clone()),
            Property::new("Processor", metrics.cpu_brand.clone()),
            numeric("Cores", metrics.cores.len().to_string()),
            numeric("Load average", load),
            numeric("Processes", process_count.to_string()),
            numeric("Threads", thread_count.to_string()),
            numeric("Uptime", fmt_uptime(metrics.uptime_secs)),
        ]))],
    )
}

/// The dashboard: processor, memory, network and disk sections in a grid
/// that reflows by width, then the system's facts.
fn dashboard(
    metrics: &Metrics,
    process_count: usize,
    thread_count: usize,
    theme: &Theme,
) -> Column {
    let network = throughput_section(
        "Network",
        &[
            Flow {
                history: metrics.net_rx_hist.as_slice(),
                rate: metrics.net_rx,
                name: "Received",
            },
            Flow {
                history: metrics.net_tx_hist.as_slice(),
                rate: metrics.net_tx,
                name: "Sent",
            },
        ],
        2,
        theme,
    );
    let disk = throughput_section(
        "Disk",
        &[
            Flow {
                history: metrics.disk_read_hist.as_slice(),
                rate: metrics.disk_read,
                name: "Read",
            },
            Flow {
                history: metrics.disk_write_hist.as_slice(),
                rate: metrics.disk_write,
                name: "Written",
            },
        ],
        4,
        theme,
    );
    let mut sections = [
        (Stat::Cpu, processor_section(metrics, theme)),
        (Stat::Memory, memory_section(metrics, theme)),
        (Stat::Network, network),
        (Stat::Disk, disk),
    ]
    .into_iter()
    .map(|(stat, section)| {
        Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_id(stat.section_id())
                .push(Box::new(section)),
        ) as Element
    });
    let mut page = Column::new()
        .with_intrinsic_height()
        .with_gap(theme.tokens.spacing.rhythm.group);
    // Two sections abreast where they fit, so the grid stays square.
    for _ in 0..2 {
        let pair: Vec<Element> = sections.by_ref().take(2).collect();
        page = page.push(Box::new(
            CardGrid::new(pair).with_card_width(CardWidth::Wide),
        ));
    }
    page.push(Box::new(system_section(
        metrics,
        process_count,
        thread_count,
    )))
}

/// Find the rect of the descendant whose test id is `id`.
fn find_rect(widget: &dyn Widget, id: &str) -> Option<Rect> {
    if widget.test_id() == Some(id) {
        return Some(widget.get_rect());
    }
    let mut found = None;
    widget.for_each_child(&mut |child| {
        if found.is_none() {
            found = find_rect(child, id);
        }
    });
    found
}

/// The Resources tab: the system dashboard, scrolling as one page.
pub(crate) struct ResourcesPane {
    shared: Shared,
    seen: Option<u64>,
    /// The theme colours the dashboard was built with.
    built_with: Option<(vello::peniko::Color, vello::peniko::Color)>,
    page: Option<Scrollable>,
    offset: Rc<Cell<(f32, f32)>>,
    /// A section to scroll to once laid out.
    reveal: Option<Stat>,
    cursor: Point,
    rect: Rect,
    node: Option<NodeId>,
    structure_dirty: bool,
}

impl ResourcesPane {
    pub(crate) fn new(shared: Shared) -> Self {
        Self {
            shared,
            seen: None,
            built_with: None,
            page: None,
            offset: Rc::new(Cell::new((0.0, 0.0))),
            reveal: None,
            cursor: Point::ZERO,
            rect: Rect::ZERO,
            node: None,
            structure_dirty: true,
        }
    }

    fn adopt(&mut self) {
        let mut bridge = self.shared.borrow_mut();
        if let Some(stat) = bridge.reveal.take() {
            self.reveal = Some(stat);
            self.structure_dirty = true;
        }
        if self.seen != Some(bridge.metrics_revision) {
            self.seen = Some(bridge.metrics_revision);
            self.built_with = None;
            self.structure_dirty = true;
        }
    }

    fn rebuild(&mut self, theme: &Theme) {
        let colors = &theme.tokens.colors;
        let key = (colors.presence.series(0), colors.intent.inverted);
        if self.built_with == Some(key) && self.page.is_some() {
            return;
        }
        let bridge = self.shared.borrow();
        let Some(metrics) = bridge.metrics.as_ref() else {
            return;
        };
        let dashboard = dashboard(metrics, bridge.process_count, bridge.thread_count, theme);
        drop(bridge);
        self.page = Some(
            Scrollable::new_vertical()
                .with_semantic_key("monitor.resources")
                .with_persisted_offset(Rc::clone(&self.offset))
                .with_cursor(self.cursor)
                .with_child(Box::new(dashboard)),
        );
        self.built_with = Some(key);
    }

    /// Scroll the requested section to the top of the page.
    fn reveal_section(&mut self) {
        let Some(stat) = self.reveal else {
            return;
        };
        let Some(page) = &self.page else {
            return;
        };
        let Some(section) = find_rect(page, &stat.section_id()) else {
            return;
        };
        self.reveal = None;
        let viewport = page.content_viewport_rect();
        let (x, y) = self.offset.get();
        let target = (section.y0 - viewport.y0 + f64::from(y)).max(0.0) as f32;
        if (target - y).abs() > 0.5 {
            self.offset.set((x, target));
            self.built_with = None;
            self.structure_dirty = true;
        }
    }
}

impl Widget for ResourcesPane {
    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        self.adopt();
        if let Some(page) = &mut self.page {
            page.sync(state, path);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.adopt();
        self.rebuild(theme);
        self.structure_dirty = false;
        let children: Vec<NodeId> = self
            .page
            .as_mut()
            .map(|page| page.build_layout(taffy, theme, node_map))
            .into_iter()
            .collect();
        let node = taffy.parent_node(
            Style {
                display: taffy::Display::Flex,
                flex_direction: taffy::FlexDirection::Column,
                flex_grow: 1.0,
                size: Size {
                    width: percent(1.0_f32),
                    height: percent(1.0_f32),
                },
                ..Default::default()
            },
            &children,
        );
        self.node = children.first().copied();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = rect_of(taffy, node, origin);
        let inner = self.rect.origin();
        if let (Some(page), Some(child)) = (&mut self.page, self.node) {
            page.apply_layout(taffy, child, inner);
        }
        self.reveal_section();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if let Some(page) = &mut self.page {
            page.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }
        if let (Some(page), Some(child)) = (&mut self.page, self.node) {
            page.handle_event(event, &mut ctx.reborrow(ctx.path, child));
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.page.iter().map(|page| page as &dyn Widget).collect()
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        if let Some(page) = &self.page {
            visitor(page);
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.page
            .iter_mut()
            .map(|page| page as &mut dyn Widget)
            .collect()
    }

    fn wants_pointer_capture(&self) -> bool {
        self.page
            .as_ref()
            .is_some_and(Widget::wants_pointer_capture)
    }

    fn needs_animation_frame(&self) -> bool {
        self.page
            .as_ref()
            .is_some_and(Widget::needs_animation_frame)
    }

    fn is_layout_dirty(&self) -> bool {
        self.structure_dirty || self.page.as_ref().is_some_and(Widget::is_layout_dirty)
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mut dirty = std::mem::take(&mut self.structure_dirty);
        if let Some(page) = &mut self.page {
            dirty |= page.take_structure_dirty();
        }
        dirty
    }

    fn name(&self) -> &'static str {
        "ResourcesPane"
    }

    fn test_id(&self) -> Option<&str> {
        Some("monitor.resources-pane")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

/// What the inspector shows of the selected process.
pub(crate) struct ProcessDetail<'a> {
    pub process: &'a ProcessInfo,
    pub parent: Option<String>,
    pub cpu_history: Vec<f32>,
    /// The CPU share and memory of the process with its descendants, when
    /// the table shows summed subtrees and the process has children: the
    /// numbers its row shows.
    pub with_children: Option<(f32, u64)>,
    /// The inset of the property list's rows, so the tiles and buttons above
    /// and below it start where its labels do.
    pub inset: f32,
    /// The process's published resource; its Stop and Kill actions are what
    /// the buttons invoke.
    pub resource: Resource,
    pub control: Rc<RefCell<Option<ControlInvocation>>>,
}

/// `child` inset by `inset` on both sides.
fn inset_by(child: Element, inset: f32) -> Element {
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_horizontal_padding(inset, inset)
            .push(child),
    )
}

/// The inspector: the selected process's load, its facts and the actions
/// that end it, or one empty state when nothing is selected.
pub(crate) fn process_detail(detail: Option<&ProcessDetail<'_>>) -> Element {
    let Some(detail) = detail else {
        return Box::new(EmptyState::new("No process selected"));
    };
    let process = detail.process;

    let mut cpu = StatTile::new("CPU", format!("{:.1}", process.cpu)).unit("%");
    if !detail.cpu_history.is_empty() {
        cpu = cpu.sparkline(detail.cpu_history.clone());
    }
    let (memory, unit) = value_and_unit(&format_bytes(process.mem_bytes));
    let tiles = StatGroup::new(vec![
        Box::new(cpu),
        Box::new(StatTile::new("Memory", memory).unit(unit)),
    ]);

    let mut properties = Vec::new();
    if let Some((cpu, memory)) = detail.with_children {
        properties.extend([
            Property::new("CPU with children", format!("{cpu:.1}%")).with_kind(TextKind::Numeric),
            Property::new("Memory with children", format_bytes(memory))
                .with_kind(TextKind::Numeric),
        ]);
    }
    let copy_control = Rc::clone(&detail.control);
    let copy_address = ActionAddress::new(&detail.resource.id, "copy-pid");
    properties.extend([
        Property::new("PID", process.pid.to_string())
            .with_kind(TextKind::Numeric)
            .action(Icon::Copy, "Copy PID", move || {
                *copy_control.borrow_mut() = Some(ControlInvocation {
                    address: copy_address.clone(),
                    arguments: serde_json::Map::new(),
                });
            }),
        Property::new("User", process.user.clone()).with_kind(TextKind::Identifier),
        Property::new("Status", process.status.clone()),
    ]);
    if let Some(parent) = &detail.parent {
        properties.push(Property::new("Parent", parent.clone()));
    }
    properties.extend([
        Property::new("Threads", process.threads.to_string()).with_kind(TextKind::Numeric),
        Property::new("Virtual memory", format_bytes(process.virt_bytes))
            .with_kind(TextKind::Numeric),
    ]);
    if let Some(files) = process.open_files {
        properties
            .push(Property::new("Open files", files.to_string()).with_kind(TextKind::Numeric));
    }
    properties.extend([
        Property::new("Running for", fmt_uptime(process.run_secs)).with_kind(TextKind::Numeric),
        Property::new("Command", process.cmd.clone()).with_kind(TextKind::Code),
    ]);

    let mut actions = Row::new().no_wrap();
    for (id, label, action, role) in [
        (
            "monitor.stop",
            "Stop process",
            "stop",
            ButtonRole::Secondary,
        ),
        (
            "monitor.kill",
            "Kill process",
            "kill",
            ButtonRole::Destructive,
        ),
    ] {
        let mut button = Button::new(id, label, None).role(role);
        if let Ok(binding) = ActionBinding::unit_for(&detail.resource, action) {
            button = button.on_action(binding);
        }
        actions = actions.push(Box::new(button));
    }

    let body = Column::new()
        .with_intrinsic_height()
        .push(inset_by(Box::new(tiles), detail.inset))
        .push(Box::new(PropertyList::new(properties)))
        .push(inset_by(Box::new(actions), detail.inset));
    Box::new(
        Scrollable::new_vertical()
            .with_semantic_key("monitor.process-detail")
            .with_child(Box::new(body)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cadence_reads_in_words() {
        let labels: Vec<(String, String)> = Cadence::all()
            .into_iter()
            .map(|cadence| (cadence.label(), cadence.short_label()))
            .collect();
        assert_eq!(labels.len(), INTERVALS.len() + 1);
        assert_eq!(
            labels[0],
            ("Every second".to_owned(), "Every 1 s".to_owned())
        );
        assert_eq!(
            labels[1],
            ("Every 1.5 seconds".to_owned(), "Every 1.5 s".to_owned())
        );
        assert_eq!(labels[5], ("Paused".to_owned(), "Paused".to_owned()));
    }

    #[test]
    fn throughput_scales_end_on_a_round_number_above_the_peak() {
        for (peak, top) in [(5.1e6, 8e6), (3.4e6, 8e6), (2.9e6, 4e6), (0.0, 2.0)] {
            assert!((axis_top(peak) - top).abs() < 1.0, "{peak} → {top}");
            let below_top = top * (GRID_DIVISIONS - 1) as f32 / GRID_DIVISIONS as f32;
            assert!(peak <= below_top, "the peak stays under the label band");
        }
    }

    #[test]
    fn values_split_from_their_units() {
        assert_eq!(
            value_and_unit("8.6 GiB"),
            ("8.6".to_owned(), "GiB".to_owned())
        );
        assert_eq!(
            value_and_unit("2.4 MB/s"),
            ("2.4".to_owned(), "MB/s".to_owned())
        );
        assert_eq!(value_and_unit("12"), ("12".to_owned(), String::new()));
    }
    fn processes_at(width: f32, scale: f32) -> ui::headless::Framed<ProcessesPane> {
        let shared = Rc::new(RefCell::new(Bridge {
            revision: 1,
            rows_revision: 1,
            total: 200,
            options: Some(ListOptions {
                sort: SortKey::Pid,
                descending: false,
                tree: false,
                sum: SumMode::PerProcess,
                kernel_threads: false,
                confirm_kill: true,
            }),
            rows: (1..=200)
                .map(|pid| ProcessRow {
                    pid,
                    started_at: 1000 + u64::from(pid),
                    name: format!("worker-{pid}"),
                    user: "person".into(),
                    cpu: 1.0,
                    mem_bytes: 1024,
                    threads: 1,
                    depth: 0,
                    has_children: false,
                    expanded: false,
                })
                .collect(),
            ..Bridge::default()
        }));
        ui::headless::Framed::new(
            ui::State::new(),
            ProcessesPane::new(shared),
            ui::headless::viewport((width, 520.0)),
            Theme::dark().with_text_scale(scale),
        )
    }

    fn pointer<W: Widget + 'static>(frame: &mut ui::headless::Framed<W>, point: Point) {
        ui::headless::Framed::harness_mut(frame).send(&WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(point.x, point.y),
        });
    }

    fn mouse<W: Widget + 'static>(frame: &mut ui::headless::Framed<W>, state: ElementState) {
        ui::headless::Framed::harness_mut(frame).send(&WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state,
            button: MouseButton::Left,
        });
    }

    fn wheel<W: Widget + 'static>(frame: &mut ui::headless::Framed<W>) {
        ui::headless::Framed::harness_mut(frame).send(&WindowEvent::MouseWheel {
            device_id: winit::event::DeviceId::dummy(),
            delta: winit::event::MouseScrollDelta::LineDelta(0.0, -3.0),
            phase: winit::event::TouchPhase::Moved,
        });
        ui::headless::Framed::harness_mut(frame).relayout();
    }

    #[test]
    fn density_and_table_rows_follow_measured_metrics_and_keep_readable_targets() {
        for scale in [1.0, 1.5, 2.0] {
            let pane = processes_at(900.0, scale);
            let semantics = pane.table.accessibility().unwrap();
            let rows: Vec<_> = semantics
                .children
                .iter()
                .filter(|node| {
                    node.action_target
                        .as_deref()
                        .is_some_and(|target| target.starts_with("row:"))
                })
                .collect();
            assert!(rows.len() >= 2);
            for pair in rows.windows(2) {
                let a = pair[0].bounds.unwrap().to_rect();
                let b = pair[1].bounds.unwrap().to_rect();
                assert!(a.height() >= 24.0);
                assert!(b.y0 >= a.y1 - 0.5, "rows do not overlap at scale {scale}");
            }
            assert!(pane.row_height >= 24.0);
            assert!(pane.table.get_rect().width() <= pane.get_rect().width());
        }
    }

    #[test]
    fn table_columns_and_toolbar_search_fit_down_to_very_narrow_widths() {
        for width in [360.0, 430.0, 800.0] {
            let pane = processes_at(width, 1.0);
            let table = pane.table.get_rect();
            let columns: f32 = pane
                .table
                .columns()
                .iter()
                .filter_map(|column| pane.table.column_width_px(&column.key))
                .sum();
            assert!(
                f64::from(columns) <= table.width() + 1.0,
                "columns fit at {width}"
            );
            let field = pane.field.get_rect();
            assert!(field.width() > 0.0 && field.x0 >= 0.0 && field.x1 <= f64::from(width) + 1.0);
        }
    }

    #[test]
    fn wheel_and_scrollbars_scroll_rows_and_page() {
        let mut pane = processes_at(900.0, 1.0);
        let center = pane.table.get_rect().center();
        pointer(&mut pane, center);
        wheel(&mut pane);
        assert!(
            pane.table.scroll_y() > 0.0,
            "wheel scrolls the virtual rows"
        );
        pane.table.set_scroll_y(0.0);
        ui::headless::Framed::harness_mut(&mut pane).relayout();
        let thumb = pane
            .table
            .scrollbar_thumb()
            .expect("overflowing rows have a scrollbar");
        let selected = pane.table.selection().primary();
        let down = Point::new(
            thumb.center().x,
            (thumb.y1 + 20.0).min(pane.table.get_rect().y1 - 8.0),
        );
        pointer(&mut pane, down);
        mouse(&mut pane, ElementState::Pressed);
        mouse(&mut pane, ElementState::Released);
        ui::headless::Framed::harness_mut(&mut pane).relayout();
        assert!(pane.table.scroll_y() > 0.0, "track click pages the rows");
        assert_eq!(
            pane.table.selection().primary(),
            selected,
            "scrollbar does not select the row behind it"
        );
        let thumb = pane.table.scrollbar_thumb().unwrap();
        pointer(&mut pane, thumb.center());
        mouse(&mut pane, ElementState::Pressed);
        assert!(pane.wants_pointer_capture());
        let end = Point::new(thumb.center().x, pane.table.get_rect().y1 - 10.0);
        let before = pane.table.scroll_y();
        pointer(&mut pane, end);
        mouse(&mut pane, ElementState::Released);
        assert!(pane.table.scroll_y() > before);
        assert!(!pane.wants_pointer_capture());

        let mut metrics = crate::model::MonitorModel::sample().metrics;
        metrics.cores = vec![20.0; 24];
        let shared = Rc::new(RefCell::new(Bridge {
            revision: 1,
            metrics_revision: 1,
            metrics: Some(metrics),
            ..Bridge::default()
        }));
        let mut resources = ui::headless::Framed::new(
            ui::State::new(),
            ResourcesPane::new(shared),
            ui::headless::viewport((900.0, 360.0)),
            Theme::dark(),
        );
        assert!(resources.page.as_ref().unwrap().max_offset_y() > 0.0);
        let center = resources.get_rect().center();
        pointer(&mut resources, center);
        wheel(&mut resources);
        assert!(
            resources.page.as_ref().unwrap().offset_y() > 0.0,
            "dashboard wheel scrolls the full page"
        );
    }

    #[test]
    fn process_table_targets_reject_a_reused_pid() {
        let mut pane = processes_at(900.0, 1.0);
        let target = format!("row:{}:0", process_row_key(1, 1001));
        let mut state = ui::State::new();
        assert!(pane.table.perform_semantic_action(
            &target,
            &ui::a11y::Action::Activate,
            &mut state,
            &ui::automerge::ROOT
        ));
        pane.shared.borrow_mut().rows[0].started_at += 1;
        pane.shared.borrow_mut().rows_revision += 1;
        pane.shared.borrow_mut().revision += 1;
        ui::headless::Framed::harness_mut(&mut pane).relayout();
        assert!(!pane.table.perform_semantic_action(
            &target,
            &ui::a11y::Action::Activate,
            &mut state,
            &ui::automerge::ROOT
        ));
    }
}
