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

use taffy::style_helpers::{length, percent};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use ui::charts::{BarGauge, Chart, ChartStyle, Series, nice_step};
use ui::format::bytes as format_bytes;
use ui::presence::PresenceCtx;
use ui::text_render::{TextKind, TextRenderer};
use ui::theme::Theme;
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
    /// Show the process row with this id (a search hit, an agent's `open`).
    Open(String),
    /// Enter or a double click on a row.
    Activate(u32),
    SetExpanded(u32, bool),
    TreeMode(bool),
    SumChildren(bool),
    KernelThreads(bool),
    /// The header's cadence button was pressed.
    ChooseCadence,
    ShowStat(Stat),
    ContextMenu {
        pid: u32,
        at: Point,
        /// The row the menu acts on, marked while the menu is open.
        row: Rect,
    },
    /// Sample the machine now: Refresh, or a retry of an unreadable list.
    Refresh,
    /// The inspector's Stop process.
    Stop(u32),
    /// The inspector's Kill process.
    Kill(u32),
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
    pub actions: Option<ui::framework::Actions<crate::resource::Monitor>>,
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
                .on_view(format!("monitor.summary.{}", stat.id()), move || {
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

/// The table's shape: it is rebuilt when one of these changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TableShape {
    tree: bool,
    /// A narrow window drops the thread and PID columns.
    compact: bool,
}

/// The process table for `shape`, and the sort key of each of its columns.
fn process_table(shape: TableShape, shared: &Shared) -> (DataTable<ProcessRow>, Vec<SortKey>) {
    let mut columns = vec![(
        SortKey::Name,
        ColumnDef::new("name", "Name", ColumnWidth::Flex(1.0))
            .with_text(|row: &ProcessRow| row.name.clone()),
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
    let menus = Rc::clone(shared);
    let table = DataTable::new(columns)
        .with_id("monitor.processes")
        .with_accessible_name("Processes")
        .selectable(true)
        .with_row_key(|row: &ProcessRow| u64::from(row.pid))
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
        })
        .on_row_context_menu(move |index, at, row| {
            let pid = menus.borrow().rows.get(index).map(|process| process.pid);
            if let Some(pid) = pid {
                menus
                    .borrow_mut()
                    .push(Request::ContextMenu { pid, at, row });
            }
        });
    if !shape.tree {
        return (table, keys);
    }
    // A tree: the table indents each process under its parent and folds a
    // parent by its chevron, or by Right and Left on the selected row.
    let folds = Rc::clone(shared);
    let table = table.tree(
        |row: &ProcessRow| row.depth,
        |row| row.has_children,
        |row| row.expanded,
        move |index, open| {
            let pid = folds.borrow().rows.get(index).map(|process| process.pid);
            if let Some(pid) = pid {
                folds.borrow_mut().push(Request::SetExpanded(pid, open));
            }
        },
    );
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
    let mut toolbar =
        Toolbar::new(Orientation::Horizontal).push_toggle("Tree view", options.tree, move || {
            tree.borrow_mut().push(Request::TreeMode(!options.tree));
        });
    let binding = shared.borrow().actions.as_ref().and_then(|actions| {
        let flat = crate::resource::Monitor::paths().flat.set(options.tree);
        actions.invocation(ui::tilo_resource::Invoke::edit(&flat).ok()?)
    });
    toolbar = match binding {
        Some(binding) => toolbar.with_action(binding),
        None => toolbar.with_view("monitor.tree-mode"),
    };
    if options.tree {
        let summing = options.sum == SumMode::SumSubtree;
        toolbar = toolbar
            .push_toggle("Include children", summing, move || {
                sum.borrow_mut().push(Request::SumChildren(!summing));
            })
            .with_view("monitor.sum-children");
    }
    toolbar
        .push_separator()
        .push_toggle("Kernel threads", options.kernel_threads, move || {
            kernel
                .borrow_mut()
                .push(Request::KernelThreads(!options.kernel_threads));
        })
        .with_effect("monitor.kernel-threads")
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
    taffy
        .layout(node)
        .map_or(Rect::ZERO, |layout| ui::widget::placed(layout, origin))
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
    nodes: Nodes,
    layout_dirty: bool,
    structure_dirty: bool,
}

impl ProcessesPane {
    pub(crate) fn new(shared: Shared) -> Self {
        let shape = TableShape {
            tree: true,
            compact: false,
        };
        let (table, sort_keys) = process_table(shape, &shared);
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
        self.table
            .set_sort_indicator(column_key(options.sort), !options.descending);
    }

    fn rebuild_table(&mut self) {
        let rows = self.table.view_rows().into_iter().cloned().collect();
        let (table, keys) = process_table(self.shape, &self.shared);
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

    /// Scroll the table so the selected row is in view.
    fn scroll_to_selection(&mut self) {
        let Some(index) = self.table.selection().primary() else {
            return;
        };
        self.table.reveal_row(index);
        self.layout_dirty = true;
    }

    /// `pid`'s row as its selection lift covers it, once the rows are laid
    /// out.
    fn row_rect(&self, pid: u32) -> Option<Rect> {
        if self.table.get_rect().width() <= 0.0 || self.viewport <= 0.0 {
            return None;
        }
        let index = self
            .table
            .view_rows()
            .iter()
            .position(|row| row.pid == pid)?;
        self.table.row_rect(index)
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

    /// Route an event to the table and adopt the selection it moved to.
    fn route_to_table(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
        child: NodeId,
    ) {
        self.table
            .handle_event(event, &mut ctx.reborrow(ctx.path, child));
        self.queue_selection();
    }
}

impl Widget for ProcessesPane {
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
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
                    _ => Request::Refresh,
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

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
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
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
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
    /// Where the Stop and Kill buttons ask for the process's effects, which
    /// the host confirms like the palette's and an agent's.
    pub requests: Shared,
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
    properties.extend([
        Property::new("PID", process.pid.to_string())
            .with_kind(TextKind::Numeric)
            .copyable(),
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
    let pid = process.pid;
    for (id, label, request, role) in [
        (
            "monitor.stop",
            "Stop process",
            Request::Stop(pid),
            ButtonRole::Secondary,
        ),
        (
            "monitor.kill",
            "Kill process",
            Request::Kill(pid),
            ButtonRole::Destructive,
        ),
    ] {
        let requests = Rc::clone(&detail.requests);
        let button = Button::new(id, label)
            .role(role)
            .on_view(move || requests.borrow_mut().push(request.clone()));
        actions = actions.push(Box::new(button));
    }

    let body = Column::new()
        .with_intrinsic_height()
        .push(Box::new(tiles))
        .push(Box::new(PropertyList::new(properties)))
        .push(Box::new(actions));
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
}
