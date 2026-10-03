//! The Logs window's widgets.
//!
//! The window is one [`ui::widget::NavShell`]: the journal table with its
//! filters is the content, the selected entry is the inspector, and the
//! content header holds the journal's commands. [`JournalPane`] owns the
//! filter field, the facet dropdowns and the table; [`crate::app::LogsApp`]
//! owns the journal and the shared view state. The two meet in a
//! [`Bridge`]: the app publishes what the pane shows, the pane queues what
//! the person asked for, and the app turns each request into a document
//! action.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use taffy::style_helpers::{length, percent};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::Key;

use ui::presence::PresenceCtx;
use ui::text_render::{TextKind, TextRenderer};
use ui::theme::Theme;
use ui::tilo_actions::ActionBinding;
use ui::widget::data_table::{ColumnDef, ColumnWidth, DataTable};
use ui::widget::{
    AppModel, BadgeFace, ChipStyle, Dropdown, Element, EmptyState, FilterCount, ModelPath,
    SearchField, TaffyTreeExt, Widget,
};
use ui::{LocalState, TextRole};

use crate::journal::JournalLoadError;
use crate::model::{Filter, LogEntry, Priority};

/// The dropdown value that means "no constraint" on a facet.
const ALL: &str = "all";

/// One row of the journal table: what the list shows of an entry.
#[derive(Clone, Debug)]
pub(crate) struct JournalRow {
    pub entry_id: String,
    pub key: u64,
    pub priority: Priority,
    pub time: String,
    pub unit: String,
    pub message: String,
}

impl JournalRow {
    pub(crate) fn of(entry: &LogEntry, entry_id: String) -> Self {
        Self {
            key: stable_key(&entry_id),
            entry_id,
            priority: entry.priority,
            time: entry.list_time(),
            unit: entry.unit_short().to_owned(),
            message: entry.message.clone(),
        }
    }
}

fn stable_key(entry_id: &str) -> u64 {
    let hex = entry_id.rsplit('.').next().unwrap_or_default();
    hex.get(..16)
        .and_then(|prefix| u64::from_str_radix(prefix, 16).ok())
        .unwrap_or_default()
}

/// What the person asked the journal view for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Request {
    Search(String),
    Unit(String),
    Boot(String),
    Priority(i64),
    Select(String),
    ClearFilters,
}

/// What the journal shows in place of rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Placeholder {
    Loading,
    Unavailable(JournalLoadError),
    Empty,
    NoMatches,
}

/// The state the app and the journal pane share.
#[derive(Default)]
pub(crate) struct Bridge {
    /// Bumped whenever anything below changes.
    pub revision: u64,
    /// Bumped when `rows` changes.
    pub rows_revision: u64,
    pub rows: Vec<JournalRow>,
    /// The selected row, as an index into `rows`.
    pub selected: Option<usize>,
    pub filter: Filter,
    pub units: Vec<String>,
    /// `(boot id, label)`, newest first.
    pub boots: Vec<(String, String)>,
    pub placeholder: Option<Placeholder>,
    /// The journal holds entries at all (the filters have something to
    /// narrow).
    pub has_entries: bool,
    /// How many entries the journal holds, filtered or not.
    pub total: usize,
    /// The journal's refresh action, offered by the unavailable state.
    pub refresh: Option<ActionBinding<()>>,
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

/// The severity dropdown's choices: each threshold, most verbose first.
fn severity_options() -> Vec<(String, String)> {
    Priority::ALL
        .iter()
        .rev()
        .map(|p| (p.level().to_string(), p.threshold_label().to_owned()))
        .collect()
}

/// Paint a row's severity badge, leading in its cell and centred on the row.
fn paint_severity(
    row: &JournalRow,
    scene: &mut Scene,
    theme: &Theme,
    text: &mut TextRenderer,
    cell: Rect,
) {
    let label = row.priority.long();
    let w = BadgeFace::intrinsic_width(label, text, theme);
    let h = BadgeFace::height(theme);
    let x0 = cell.x0 + f64::from(theme.tokens.spacing.s2);
    let y0 = cell.center().y - h / 2.0;
    BadgeFace::new(Rect::new(x0, y0, x0 + w, y0 + h), label)
        .style(severity_style(row.priority))
        .paint(scene, Affine::IDENTITY, text, theme);
}

/// The badge style a severity is shown in.
pub(crate) fn severity_style(priority: Priority) -> ChipStyle {
    match priority.tone() {
        ui::widget::Tone::Neutral => ChipStyle::Neutral,
        tone => ChipStyle::Tinted(tone),
    }
}

/// The journal table. `compact` drops the unit column for a narrow window;
/// the unit stays in the inspector and in each row's spoken value.
fn journal_table(level_width: f32, compact: bool) -> DataTable<JournalRow> {
    let mut columns = vec![
        ColumnDef::new("time", "Time", ColumnWidth::Auto)
            .with_kind(TextKind::Numeric)
            .with_text(|row: &JournalRow| row.time.clone()),
        ColumnDef::new("level", "Level", ColumnWidth::Fixed(level_width))
            .with_render(Box::new(paint_severity))
            .with_accessible_value(|row: &JournalRow| row.priority.long().to_owned()),
    ];
    if !compact {
        columns.push(
            ColumnDef::new("unit", "Unit", ColumnWidth::Auto)
                .with_text(|row: &JournalRow| row.unit.clone()),
        );
    }
    columns.push(
        ColumnDef::new("message", "Message", ColumnWidth::Flex(1.0))
            .with_text(|row: &JournalRow| row.message.clone()),
    );
    DataTable::new(columns)
        .with_id("logs.entries")
        .with_accessible_name("Journal entries")
        .selectable(true)
        .with_row_key(|row: &JournalRow| row.key)
}

/// Child node ids of the last layout.
#[derive(Default)]
struct Nodes {
    field: Option<NodeId>,
    unit: Option<NodeId>,
    boot: Option<NodeId>,
    severity: Option<NodeId>,
    body: Option<NodeId>,
    body_child: Option<NodeId>,
}

/// The table's shape: it is rebuilt when one of these changes.
#[derive(Clone, Copy, Debug, PartialEq)]
struct TableShape {
    level_width: f32,
    compact: bool,
}

/// The journal: the filter field and the facet dropdowns over the entry
/// table, or one empty state when there is nothing to list.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent layout, structure and arrangement flags"
)]
pub(crate) struct JournalPane {
    shared: Shared,
    seen: Option<u64>,
    seen_rows: Option<u64>,
    field: SearchField,
    unit: Dropdown,
    boot: Dropdown,
    severity: Dropdown,
    table: DataTable<JournalRow>,
    shape: TableShape,
    /// The viewport height last handed to the table.
    viewport: f64,
    row_height: f64,
    placeholder: Option<(Placeholder, EmptyState)>,
    has_entries: bool,
    /// The field and the dropdowns share one band.
    wide: bool,
    /// The row index the table's selection was last synchronised to.
    shown_selection: Option<usize>,
    /// The selection the app last published, and whether the table still
    /// has to scroll it into view.
    adopted_selection: Option<usize>,
    reveal_selection: bool,
    rect: Rect,
    nodes: Nodes,
    layout_dirty: bool,
    structure_dirty: bool,
}

impl JournalPane {
    pub(crate) fn new(shared: Shared) -> Self {
        let unit = Self::facet(&shared, "logs.unit", "Unit", Request::Unit);
        let boot = Self::facet(&shared, "logs.boot", "Boot", Request::Boot);
        let mut severity = {
            let requests = Rc::clone(&shared);
            Dropdown::from_state(Vec::new(), &LocalState::new(String::new()))
                .with_id("logs.severity")
                .caption("Severity")
                .on_change(move |value| {
                    if let Ok(level) = value.parse::<i64>() {
                        requests.borrow_mut().push(Request::Priority(level));
                    }
                })
        };
        severity.set_value_options(severity_options());
        let shape = TableShape {
            level_width: 0.0,
            compact: false,
        };
        Self {
            shared,
            seen: None,
            seen_rows: None,
            field: SearchField::filter()
                .with_id("logs.filter")
                .with_placeholder("Filter messages and units"),
            unit,
            boot,
            severity,
            table: journal_table(1.0, false),
            shape,
            viewport: 0.0,
            row_height: 0.0,
            placeholder: None,
            has_entries: false,
            wide: true,
            shown_selection: None,
            adopted_selection: None,
            reveal_selection: false,
            rect: Rect::ZERO,
            nodes: Nodes::default(),
            layout_dirty: true,
            structure_dirty: true,
        }
    }

    fn facet(shared: &Shared, id: &str, caption: &str, request: fn(String) -> Request) -> Dropdown {
        let requests = Rc::clone(shared);
        Dropdown::from_state(Vec::new(), &LocalState::new(ALL.to_owned()))
            .with_id(id)
            .caption(caption)
            .on_change(move |value| {
                let value = if value == ALL { "" } else { value };
                requests.borrow_mut().push(request(value.to_owned()));
            })
    }

    /// Adopt what the app published since the last look.
    fn adopt(&mut self) {
        let shared = Rc::clone(&self.shared);
        let bridge = shared.borrow();
        if self.seen == Some(bridge.revision) {
            return;
        }
        self.seen = Some(bridge.revision);

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

        let mut units = vec![(ALL.to_owned(), "All units".to_owned())];
        units.extend(bridge.units.iter().map(|u| (u.clone(), u.clone())));
        self.unit.set_value_options(units);
        self.unit.set_selected(facet_value(&bridge.filter.unit));

        let mut boots = vec![(ALL.to_owned(), "All boots".to_owned())];
        boots.extend(bridge.boots.iter().cloned());
        self.boot.set_value_options(boots);
        self.boot.set_selected(facet_value(&bridge.filter.boot));

        self.severity
            .set_selected(bridge.filter.min_priority.clamp(0, 7).to_string());

        if self.field.value() != bridge.filter.search {
            self.field.set_query(&bridge.filter.search);
        }
        // "12 of 96" while any filter narrows the journal.
        self.field.set_count(
            (!bridge.filter.is_clear()).then(|| FilterCount::new(bridge.rows.len(), bridge.total)),
        );

        let placeholder = bridge.placeholder;
        if self.placeholder.as_ref().map(|(kind, _)| *kind) != placeholder {
            self.placeholder = placeholder.map(|kind| (kind, empty_state(kind, &bridge)));
            self.structure_dirty = true;
        }
        if self.has_entries != bridge.has_entries {
            self.has_entries = bridge.has_entries;
            self.structure_dirty = true;
        }
        self.layout_dirty = true;
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
    /// arrow keys), as the shared selection.
    fn queue_selection(&mut self) {
        let primary = self.table.selection().primary();
        if primary == self.shown_selection {
            return;
        }
        self.shown_selection = primary;
        let Some(index) = primary else {
            return;
        };
        let entry = self
            .table
            .view_rows()
            .get(index)
            .map(|row| row.entry_id.clone());
        if let Some(entry_id) = entry {
            self.shared.borrow_mut().push(Request::Select(entry_id));
        }
    }

    /// Rebuild the table when its shape changes, keeping rows and selection.
    fn ensure_table(&mut self, shape: TableShape) {
        if shape == self.shape {
            return;
        }
        self.shape = shape;
        let rows = self.table.view_rows().into_iter().cloned().collect();
        let selected = self.table.selection().primary();
        self.table = journal_table(shape.level_width, shape.compact);
        self.table.set_rows(rows);
        if let Some(index) = selected {
            self.table.selection_mut().select_only(index);
        }
        self.viewport = 0.0;
        self.structure_dirty = true;
    }

    /// The filter field and the facet dropdowns: one band on a wide pane,
    /// the field over the dropdowns on a narrow one.
    fn build_filters(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> Vec<NodeId> {
        let tokens = &theme.tokens;
        let gutter = tokens.spacing.s3;
        let gap = tokens.spacing.s2;
        let band = |bottom: f32| Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Row,
            flex_shrink: 0.0,
            align_items: Some(taffy::AlignItems::FlexEnd),
            gap: Size {
                width: length(gap),
                height: length(gap),
            },
            padding: taffy::Rect {
                left: length(gutter),
                right: length(gutter),
                top: length(gap),
                bottom: length(bottom),
            },
            size: Size {
                width: percent(1.0_f32),
                height: Dimension::auto(),
            },
            ..Default::default()
        };

        let field = self.field.build_layout(taffy, theme, node_map);
        let unit = self.unit.build_layout(taffy, theme, node_map);
        let boot = self.boot.build_layout(taffy, theme, node_map);
        let severity = self.severity.build_layout(taffy, theme, node_map);
        for facet in [unit, boot, severity] {
            flexed(taffy, facet, 1.0);
        }
        let bands = if self.wide {
            flexed(taffy, field, 2.0);
            vec![taffy.parent_node(band(gap), &[field, unit, boot, severity])]
        } else {
            flexed(taffy, field, 1.0);
            vec![
                taffy.parent_node(band(0.0), &[field]),
                taffy.parent_node(band(gap), &[unit, boot, severity]),
            ]
        };
        self.nodes.field = Some(field);
        self.nodes.unit = Some(unit);
        self.nodes.boot = Some(boot);
        self.nodes.severity = Some(severity);
        bands
    }

    /// Scroll the table so the selected row is in view, centred when it was
    /// out of sight.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        reason = "row offsets are small logical-pixel values"
    )]
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

    fn shows_filters(&self) -> bool {
        self.has_entries
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
}

fn facet_value(value: &str) -> String {
    if value.is_empty() {
        ALL.to_owned()
    } else {
        value.to_owned()
    }
}

/// One sentence and at most one action for each way the list is empty.
fn empty_state(kind: Placeholder, bridge: &Bridge) -> EmptyState {
    match kind {
        Placeholder::Loading => EmptyState::new("Reading the system journal"),
        Placeholder::Unavailable(error) => {
            let state = EmptyState::error("Journal unavailable", error.message());
            match &bridge.refresh {
                Some(refresh) => state.with_action("Try again").on_action(refresh.clone()),
                None => state,
            }
        }
        Placeholder::Empty => EmptyState::new("The journal has no entries yet"),
        Placeholder::NoMatches => {
            EmptyState::new("No entries match these filters").with_action("Clear filters")
        }
    }
}

/// `style` grown or shrunk along a band's main axis.
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

impl Widget for JournalPane {
    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        self.adopt();
        for child in self.children_mut() {
            child.sync(state, path);
        }
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "spacing tokens and widths are small logical-pixel values"
    )]
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.adopt();
        let tokens = &theme.tokens;
        let metrics = tokens.metrics;
        let growth = theme.text_growth();
        // Wide enough for "Emergency" in a badge, at this text size.
        let level_width = (tokens.spacing.s16 + tokens.spacing.s8) * growth;
        let compact = self.rect.width() > 0.0
            && self.rect.width() < f64::from(metrics.content_min_width + metrics.detail_min_width);
        self.ensure_table(TableShape {
            level_width,
            compact,
        });
        self.wide = self.rect.width() <= 0.0
            || self.rect.width() >= 2.0 * f64::from(metrics.content_min_width * growth);
        self.row_height = f64::from(metrics.min_pointer_target);
        self.layout_dirty = false;

        self.nodes = Nodes::default();
        let mut children = if self.shows_filters() {
            self.build_filters(taffy, theme, node_map)
        } else {
            Vec::new()
        };

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
                // The rows' selection lift lines up with the filter field.
                padding: taffy::Rect {
                    left: length(theme.tokens.spacing.s1),
                    right: length(theme.tokens.spacing.s1),
                    top: length(0.0_f32),
                    bottom: length(0.0_f32),
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

    #[allow(
        clippy::cast_possible_truncation,
        reason = "viewport heights are small logical-pixel values"
    )]
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
        let band_of = |child: Option<NodeId>| {
            child
                .and_then(|c| taffy.parent(c))
                .map_or(inner, |band| origin_of(taffy, band, inner))
        };
        if let Some(field) = self.nodes.field {
            self.field.apply_layout(taffy, field, band_of(Some(field)));
        }
        if let Some(unit) = self.nodes.unit {
            self.unit.apply_layout(taffy, unit, band_of(Some(unit)));
        }
        if let Some(boot) = self.nodes.boot {
            self.boot.apply_layout(taffy, boot, band_of(Some(boot)));
        }
        if let Some(severity) = self.nodes.severity {
            self.severity
                .apply_layout(taffy, severity, band_of(Some(severity)));
        }
        if let (Some(body), Some(child)) = (self.nodes.body, self.nodes.body_child) {
            let body_origin = origin_of(taffy, body, inner);
            let available = taffy
                .layout(body)
                .map_or(0.0, |layout| f64::from(layout.size.height));
            self.body_mut().apply_layout(taffy, child, body_origin);
            if self.placeholder.is_none() {
                // The table is as tall as its header plus the viewport it was
                // given; hand it the viewport that fills the body.
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
                }
            }
        }
        if width_changed {
            // The arrangement of the bands and the table's columns follow
            // the width; the next layout re-decides them.
            self.layout_dirty = true;
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
        if self.shows_filters() {
            self.field
                .draw(scene, transform, text_renderer, theme, presence);
            self.unit
                .draw(scene, transform, text_renderer, theme, presence);
            self.boot
                .draw(scene, transform, text_renderer, theme, presence);
            self.severity
                .draw(scene, transform, text_renderer, theme, presence);
        }
        self.body_mut()
            .draw(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if self.shows_filters()
            && let WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Character(c),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } = event
            && c.eq_ignore_ascii_case("f")
            && ctx.modifiers.state().control_key()
            && !ctx.modifiers.state().shift_key()
            && let Some(field) = self.nodes.field
        {
            let input = ctx
                .taffy
                .children(field)
                .unwrap_or_default()
                .first()
                .copied();
            *ctx.focus_target = Some(input.unwrap_or(field));
            self.field.select_all();
            *ctx.handled = true;
            return;
        }
        if self.shows_filters() {
            // An open list floats over the table: the dropdowns go first.
            for (dropdown, node) in [
                (&mut self.unit, self.nodes.unit),
                (&mut self.boot, self.nodes.boot),
                (&mut self.severity, self.nodes.severity),
            ] {
                if let Some(node) = node {
                    dropdown.handle_event(event, &mut ctx.reborrow(ctx.path, node));
                    if *ctx.handled {
                        return;
                    }
                }
            }
            if let Some(field) = self.nodes.field {
                self.field
                    .handle_event(event, &mut ctx.reborrow(ctx.path, field));
                if let Some(query) = self.field.take_dirty() {
                    self.shared.borrow_mut().push(Request::Search(query));
                }
                if *ctx.handled {
                    return;
                }
            }
        }
        if let Some(child) = self.nodes.body_child {
            if let Some((kind, empty)) = &mut self.placeholder {
                empty.handle_event(event, &mut ctx.reborrow(ctx.path, child));
                if *kind == Placeholder::NoMatches && empty.take_action() {
                    self.shared.borrow_mut().push(Request::ClearFilters);
                }
            } else {
                self.table
                    .handle_event(event, &mut ctx.reborrow(ctx.path, child));
                self.queue_selection();
            }
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        let mut children: Vec<&dyn Widget> = Vec::new();
        if self.shows_filters() {
            children.extend([
                &self.field as &dyn Widget,
                &self.unit,
                &self.boot,
                &self.severity,
            ]);
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
        if shows_filters {
            children.push(&mut self.field);
            children.push(&mut self.unit);
            children.push(&mut self.boot);
            children.push(&mut self.severity);
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
        "JournalPane"
    }

    fn test_id(&self) -> Option<&str> {
        Some("logs.journal")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

/// The inspector: the selected entry's severity, time, message and every
/// field, or one empty state when nothing is selected.
pub(crate) fn entry_detail(entry: Option<&LogEntry>) -> Element {
    use ui::widget::{Badge, Column, Property, PropertyList, Row, Scrollable, Text};
    let Some(entry) = entry else {
        return Box::new(EmptyState::new("No entry selected"));
    };
    // The unit is the inspector's title.
    let mut properties =
        vec![Property::new("Program", entry.identifier.clone()).with_kind(TextKind::Identifier)];
    if let Some(pid) = entry.pid {
        properties.push(Property::new("Process ID", pid.to_string()).with_kind(TextKind::Numeric));
    }
    properties.extend([
        Property::new("Host", entry.hostname.clone()).with_kind(TextKind::Identifier),
        Property::new("Transport", entry.transport.clone()),
        Property::new("Boot ID", entry.boot_id.clone()).with_kind(TextKind::Identifier),
    ]);
    for (key, value) in &entry.extra {
        if !value.trim().is_empty() {
            properties.push(Property::new(field_label(key), value.clone()));
        }
    }

    let heading = Row::new()
        .no_wrap()
        .with_align_items(taffy::AlignItems::Center)
        .push(Box::new(
            Badge::text(entry.priority.long()).style(severity_style(entry.priority)),
        ))
        .push(Box::new(
            Text::new(&entry.timestamp_full())
                .with_role(TextRole::Caption)
                .numeric()
                .dimmed(),
        ));
    let body = Column::new()
        .with_intrinsic_height()
        .push(Box::new(heading))
        .push(Box::new(
            Text::new(&entry.message).with_role(TextRole::Body),
        ))
        .push(Box::new(PropertyList::new(properties)));
    Box::new(
        Scrollable::new_vertical()
            .with_semantic_key("logs.entry-detail")
            .with_child(Box::new(body)),
    )
}

/// Words of journal field names that are abbreviations, as they are
/// written.
const ABBREVIATIONS: &[&str] = &[
    "BT", "CPU", "DRM", "GID", "GPU", "ID", "IP", "NM", "PCI", "PID", "SSH", "TID", "TTL", "UID",
    "USB", "UUID",
];

/// A journal field name in sentence case: `SYSLOG_FACILITY` reads
/// "Syslog facility", `_KERNEL_DEVICE` "Kernel device", `BT_ADAPTER`
/// "BT adapter". Journal field names are upper case, so the first letter
/// already is.
pub(crate) fn field_label(key: &str) -> String {
    key.trim_start_matches('_')
        .split('_')
        .filter(|word| !word.is_empty())
        .enumerate()
        .map(|(index, word)| {
            if let Some(abbreviation) = ABBREVIATIONS
                .iter()
                .find(|abbreviation| abbreviation.eq_ignore_ascii_case(word))
            {
                (*abbreviation).to_owned()
            } else if index == 0 {
                let mut chars = word.chars();
                chars.next().map_or_else(String::new, |first| {
                    std::iter::once(first)
                        .chain(chars.as_str().to_lowercase().chars())
                        .collect()
                })
            } else {
                word.to_lowercase()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_labels_read_in_sentence_case() {
        for (key, label) in [
            ("SYSLOG_FACILITY", "Syslog facility"),
            ("_KERNEL_DEVICE", "Kernel device"),
            ("CODE_FILE", "Code file"),
            ("PEER", "Peer"),
            ("BT_ADAPTER", "BT adapter"),
            ("_SYSTEMD_UNIT_ID", "Systemd unit ID"),
        ] {
            assert_eq!(field_label(key), label, "case {key}");
        }
    }

    #[test]
    fn severity_choices_run_from_all_levels_to_emergencies() {
        let options = severity_options();
        assert_eq!(options.len(), 8);
        assert_eq!(options[0], ("7".to_owned(), "All levels".to_owned()));
        assert_eq!(options[7], ("0".to_owned(), "Emergency only".to_owned()));
    }
}
