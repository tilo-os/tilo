//! The Logs window's widgets.
//!
//! The window is one [`ui::widget::NavShell`]: the journal table with its
//! filters is the content, the selected entry is the inspector, and the
//! content header holds the journal's commands. The shell holds the
//! window's filter field; [`JournalPane`] is what it filters and owns the
//! facet dropdowns and the table; [`crate::app::LogsApp`]
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
use winit::event::WindowEvent;

use ui::presence::PresenceCtx;
use ui::text_render::{TextKind, TextRenderer};
use ui::theme::Theme;
use ui::tilo_actions::ActionBinding;
use ui::tokens::Spacing;
use ui::widget::data_table::{ColumnDef, ColumnWidth, DataTable};
use ui::widget::{
    AppModel, ChipStyle, Dropdown, Element, EmptyState, FilterCount, Filterable, ModelPath,
    TaffyTreeExt, Widget, layout_rect,
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
/// The badge style a severity is shown in.
pub(crate) fn severity_style(priority: Priority) -> ChipStyle {
    match priority.tone() {
        ui::widget::Tone::Neutral => ChipStyle::Neutral,
        tone => ChipStyle::Tinted(tone),
    }
}

/// The journal table. The unit column gives way first in a narrow window;
/// the unit stays in the inspector and in each row's spoken value.
fn journal_table() -> DataTable<JournalRow> {
    let columns = vec![
        ColumnDef::new("time", "Time", ColumnWidth::Auto)
            .with_kind(TextKind::Numeric)
            .with_text(|row: &JournalRow| row.time.clone()),
        ColumnDef::new("level", "Level", ColumnWidth::Auto)
            .with_badge(|row: &JournalRow| (row.priority.long().to_owned(), row.priority.tone())),
        ColumnDef::new("unit", "Unit", ColumnWidth::Auto)
            .with_text(|row: &JournalRow| row.unit.clone())
            .priority(1),
        ColumnDef::new("message", "Message", ColumnWidth::Flex(1.0))
            .min_width(240.0)
            .with_text(|row: &JournalRow| row.message.clone()),
    ];
    DataTable::new(columns)
        .compact()
        .fill_height()
        .with_id("logs.entries")
        .with_accessible_name("Journal entries")
        .selectable(true)
        .with_row_key(|row: &JournalRow| row.key)
}

/// Child node ids of the last layout.
#[derive(Default)]
struct Nodes {
    unit: Option<NodeId>,
    boot: Option<NodeId>,
    severity: Option<NodeId>,
    body: Option<NodeId>,
    body_child: Option<NodeId>,
}

/// The journal: the facet dropdowns over the entry table, or one empty state
/// when there is nothing to list. The shell's filter field narrows it
/// ([`Filterable`]).
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent layout, structure and selection flags"
)]
pub(crate) struct JournalPane {
    shared: Shared,
    seen: Option<u64>,
    seen_rows: Option<u64>,
    unit: Dropdown,
    boot: Dropdown,
    severity: Dropdown,
    table: DataTable<JournalRow>,
    placeholder: Option<(Placeholder, EmptyState)>,
    has_entries: bool,
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
        Self {
            shared,
            seen: None,
            seen_rows: None,
            unit,
            boot,
            severity,
            table: journal_table(),
            placeholder: None,
            has_entries: false,
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

    /// The facet dropdowns, in one band under the shell's filter field.
    fn build_filters(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> Vec<NodeId> {
        let tokens = &theme.tokens;
        let gutter = tokens.spacing.s3;
        let gap = tokens.spacing.s2;
        let unit = self.unit.build_layout(taffy, theme, node_map);
        let boot = self.boot.build_layout(taffy, theme, node_map);
        let severity = self.severity.build_layout(taffy, theme, node_map);
        for facet in [unit, boot, severity] {
            ui::widget::share_row(taffy, facet, 1.0);
        }
        let band = taffy.parent_node(
            Style {
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
                    top: length(0.0_f32),
                    bottom: length(gap),
                },
                size: Size {
                    width: percent(1.0_f32),
                    height: Dimension::auto(),
                },
                ..Default::default()
            },
            &[unit, boot, severity],
        );
        self.nodes.unit = Some(unit);
        self.nodes.boot = Some(boot);
        self.nodes.severity = Some(severity);
        vec![band]
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

impl Filterable for JournalPane {
    /// The shell's filter field edited the query: ask the app for it.
    fn set_filter(&mut self, query: &str) {
        let mut bridge = self.shared.borrow_mut();
        if bridge.filter.search != query {
            bridge.push(Request::Search(query.to_owned()));
        }
    }

    /// "12 of 96" while any filter narrows the journal.
    fn filter_count(&self) -> Option<FilterCount> {
        let bridge = self.shared.borrow();
        (!bridge.filter.is_clear()).then(|| FilterCount::new(bridge.rows.len(), bridge.total))
    }
}

impl Widget for JournalPane {
    ui::provides!(dyn Filterable);

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
                // The columns' text stands on the filter field's edges; the
                // selection lift bleeds into this padding.
                padding: taffy::Rect {
                    left: length(theme.tokens.spacing.s3),
                    right: length(theme.tokens.spacing.s3),
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
                size: Size::from_percent(1.0_f32, 1.0_f32),
                ..Default::default()
            },
            &children,
        );
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        self.structure_dirty = false;
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let rect = layout_rect(taffy, node, origin);
        let width_changed = (rect.width() - self.rect.width()).abs() > 0.5;
        self.rect = rect;
        let inner = rect.origin();
        let band_of = |child: Option<NodeId>| {
            child
                .and_then(|c| taffy.parent(c))
                .map_or(inner, |band| layout_rect(taffy, band, inner).origin())
        };
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
            let body_origin = layout_rect(taffy, body, inner).origin();
            // The table fills the body and scrolls its own rows; a selection
            // the app moved (a search result, a restored view) is brought
            // into sight by the nearest edge once the table has its height.
            self.body_mut().apply_layout(taffy, child, body_origin);
            if self.placeholder.is_none()
                && std::mem::take(&mut self.reveal_selection)
                && let Some(index) = self.table.selection().primary()
            {
                self.table.reveal_row(index);
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
            children.extend([&self.unit as &dyn Widget, &self.boot, &self.severity]);
        }
        children.push(self.body());
        children
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let shows_filters = self.shows_filters();
        let body: &mut dyn Widget = match &mut self.placeholder {
            Some((_, empty)) => empty,
            None => &mut self.table,
        };
        let mut children: Vec<&mut (dyn Widget + '_)> = Vec::new();
        if shows_filters {
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
pub(crate) fn entry_detail(entry: Option<&LogEntry>, spacing: &Spacing) -> Element {
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
    // The severity, time, message and the property labels all start on the
    // region's gutter, under the inspector's title.
    let gutter = spacing.s6;
    let summary = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .push(Box::new(heading))
        .push(Box::new(
            Text::new(&entry.message).with_role(TextRole::Body),
        ));
    let body = Column::new()
        .with_horizontal_padding(gutter, gutter)
        .with_intrinsic_height()
        .push(Box::new(summary))
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
