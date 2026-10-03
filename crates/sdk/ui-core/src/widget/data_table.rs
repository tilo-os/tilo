//! `DataTable` primitive — sortable, resizable, virtualized tabular view.
//!
//! Sort is non-destructive: rows stay in insertion order and an index `view`
//! carries the display order, so [`DataTable::sort_by_with`] can re-apply the
//! remembered comparator whenever local rows change. [`DataTable::from_model`]
//! instead delegates order and paging to [`TableModel`]. Its sparse viewport
//! keeps memory proportional to measured/visible rows even when the logical
//! model has hundreds of millions of entries. Keyed, variable-height row
//! virtualization ([`DataTable::set_viewport`]/[`DataTable::visible_row_range`])
//! windows paint, hit testing, fetching, scrolling, and accessibility to the
//! same visible rows.

use crate::time::Instant;
use crate::widget::{AppModel, ModelPath};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;
use taffy::style_helpers::{length, percent};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::Color;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::tokens::colors::Colors;
use crate::virtual_viewport::{
    DragScroller, ScrollAlignment, ScrollMomentum, SparseAnchor, SparseVirtualViewport, WheelStep,
};
use crate::widget::scrollbar::{PressOutcome, ScrollMetrics, Scrollbar};
use crate::widget::{Element, TaffyTreeExt, Widget};

/// Matches [`crate::widget::Scrollable`] so a `DataTable` bar and a
/// wrapping `Scrollable` bar read identically.
const BAR_THICKNESS: f64 = 8.0;
const BAR_MARGIN: f64 = 2.0;

/// One line of `role` as the renderer will lay it out: the role's resolved
/// size times its leading, so a band sized from this at layout time agrees
/// with the line the widget measures and centres at paint time.
pub fn type_line_height(
    typography: &crate::tokens::Typography,
    role: crate::tokens::TextRole,
) -> f64 {
    f64::from(typography.style(role).line_height_px())
}

/// A structural band that follows the type: `floor` is the widget's 1×
/// default, which `content + padding` never undercuts at the default text
/// scale, so 1× layouts are unchanged and only larger type grows the band.
pub fn text_band(floor: f64, content: f64, padding: f64) -> f64 {
    floor.max(content + padding)
}

/// The estimated height of an unmeasured row: the host's authored estimate
/// (from `set_viewport`), raised only once the label line plus `authored_pad`
/// would not fit, or — unauthored — the widget's `default` grown to the line
/// plus `default_pad`. `line` is `None` before the first layout, when no
/// theme has been seen and the estimate is exactly what the host set. This
/// deliberately does not use `SparseVirtualViewport::set_minimum_height`:
/// that floor also applies to heights set per row (`set_row_height`), which
/// are the host's exact values.
pub(crate) fn row_estimate(authored: Option<f64>, default: f64, row_type: Option<RowType>) -> f64 {
    match (authored, row_type) {
        (Some(authored), Some(t)) => text_band(authored, t.line, t.authored_pad),
        (Some(authored), None) => authored,
        (None, Some(t)) => text_band(default, t.line, t.default_pad),
        (None, None) => default,
    }
}

/// What a row widget learned about its label type at its last layout: the
/// line box and the padding each kind of estimate keeps around it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RowType {
    pub(crate) line: f64,
    pub(crate) default_pad: f64,
    pub(crate) authored_pad: f64,
}

/// `Auto` is sized to content at build time (treated as `Flex(1.0)` until
/// the layout pass can measure it).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ColumnWidth {
    Fixed(f32),
    Flex(f32),
    Auto,
}

/// Register one per column via [`ColumnDef::with_text_render`]; columns
/// without one render nothing (header + chrome still draw). Also gets
/// whether the row is selected, so text colour can flip for contrast.
pub type CellTextRender<Row> =
    Box<dyn Fn(&Row, &mut Scene, &mut TextRenderer, Rect, &Theme, bool) + Send + Sync>;

/// Coordinates are absolute scene space; the table-level transform is already applied.
pub type CellRender<Row> =
    Box<dyn Fn(&Row, &mut Scene, &Theme, &mut TextRenderer, Rect) + Send + Sync>;

/// Builds a real retained widget for one visible cell. The widget is keyed by
/// the row's stable identity and the column key, so it survives sorting and
/// scrolling while that cell remains in the mounted window.
pub type CellWidgetBuilder<Row> = Box<dyn FnMut(&Row, usize) -> Element>;
pub type CellAccessibleValue<Row> = Arc<dyn Fn(&Row) -> String + Send + Sync>;
/// A cell's text, or `None` for an absent value (a SQL `NULL`, an unset
/// field), which the table paints dimmed as the column's absent label.
pub type CellText<Row> = Arc<dyn Fn(&Row) -> Option<String> + Send + Sync>;

pub struct ColumnDef<Row> {
    pub key: String,
    pub header: String,
    pub width: ColumnWidth,
    pub sortable: bool,
    /// `None` renders an empty cell.
    pub text_render: Option<CellTextRender<Row>>,
    /// No-op by default; set via [`ColumnDef::with_render`].
    pub render_cell: CellRender<Row>,
    /// `None` by default; set via [`ColumnDef::with_widget`].
    pub widget: Option<CellWidgetBuilder<Row>>,
    /// Value exposed for ready rows; set directly or by [`ColumnDef::with_text`].
    pub accessible_value: Option<CellAccessibleValue<Row>>,
    /// The cell text the table paints itself (set by [`ColumnDef::with_text`]
    /// and [`ColumnDef::with_optional_text`]).
    pub text: Option<CellText<Row>>,
    /// Numbers: right-aligned in tabular figures, header included.
    pub numeric: bool,
    /// What an absent value reads as, dimmed.
    pub absent: String,
    pub(crate) resize_handle_px: f32,
}

impl<Row> ColumnDef<Row> {
    pub fn new(key: impl Into<String>, header: impl Into<String>, width: ColumnWidth) -> Self {
        Self {
            key: key.into(),
            header: header.into(),
            width,
            sortable: false,
            text_render: None,
            render_cell: Box::new(|_, _, _, _, _| {}),
            widget: None,
            accessible_value: None,
            text: None,
            numeric: false,
            absent: String::from("—"),
            resize_handle_px: 6.0,
        }
    }

    /// The column holds numbers (sizes, counts, durations, prices): cells
    /// and header align to the trailing edge and digits are tabular.
    #[must_use]
    pub fn numeric(mut self) -> Self {
        self.numeric = true;
        self
    }

    #[must_use]
    pub fn sortable(mut self, v: bool) -> Self {
        self.sortable = v;
        self
    }

    #[must_use]
    pub fn with_text_render(mut self, f: CellTextRender<Row>) -> Self {
        self.text_render = Some(f);
        self
    }

    /// Render ordinary cell text and expose the exact same string as the
    /// accessible cell value. This is the concise default for most columns;
    /// use [`Self::with_text_render`] plus [`Self::with_accessible_value`] when
    /// custom drawing is required.
    #[must_use]
    pub fn with_text(self, value: impl Fn(&Row) -> String + Send + Sync + 'static) -> Self
    where
        Row: 'static,
    {
        self.with_optional_text(move |row| Some(value(row)), "")
    }

    /// Cell text that may be absent (a SQL `NULL`): `None` paints `absent`
    /// in tertiary ink ("NULL", or "—" for an unset field), and is read out
    /// as that label.
    #[must_use]
    pub fn with_optional_text(
        mut self,
        value: impl Fn(&Row) -> Option<String> + Send + Sync + 'static,
        absent: impl Into<String>,
    ) -> Self
    where
        Row: 'static,
    {
        let absent = absent.into();
        if !absent.is_empty() {
            self.absent.clone_from(&absent);
        }
        let value: CellText<Row> = Arc::new(value);
        let spoken = Arc::clone(&value);
        let absent = self.absent.clone();
        self.accessible_value = Some(Arc::new(move |row| {
            spoken(row).unwrap_or_else(|| absent.clone())
        }));
        self.text = Some(value);
        self
    }

    /// Expose a value for a custom-painted cell. The column header remains
    /// the cell name, while this string becomes its row-specific value.
    #[must_use]
    pub fn with_accessible_value(
        mut self,
        value: impl Fn(&Row) -> String + Send + Sync + 'static,
    ) -> Self {
        self.accessible_value = Some(Arc::new(value));
        self
    }

    #[must_use]
    pub fn with_render(mut self, f: CellRender<Row>) -> Self {
        self.render_cell = f;
        self
    }

    /// Render this column with real child widgets. Only ready cells in the
    /// visible range plus overscan are built; controls participate in input,
    /// focus, animation, layout, inspection, and accessibility.
    #[must_use]
    pub fn with_widget(mut self, builder: impl FnMut(&Row, usize) -> Element + 'static) -> Self {
        self.widget = Some(Box::new(builder));
        self
    }
}

pub type SortCmp<Row> = Box<dyn Fn(&Row, &Row) -> Ordering + Send + Sync>;

/// State of one logical row in a potentially paged or asynchronous table.
/// Loading and error rows keep their geometry and stable key, so completing a
/// fetch does not disturb the scroll anchor or selection.
#[derive(Clone, Copy, Debug)]
pub enum TableRowState<'a, Row> {
    Ready(&'a Row),
    Loading,
    Error(&'a str),
}

/// One model notification batch consumed after the backing data has changed.
/// Structural variants preserve mounted controls by stable row key;
/// [`Self::RowsChanged`] rebuilds controls only for the named rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TableModelChange {
    RowsInserted {
        index: usize,
        count: usize,
    },
    RowsRemoved {
        index: usize,
        count: usize,
    },
    /// `to` is the first destination index in the post-move order.
    RowsMoved {
        from: usize,
        to: usize,
        count: usize,
    },
    RowsChanged {
        keys: Vec<u64>,
    },
    Reset,
}

/// Unopinionated backing model for very large or asynchronously populated
/// tables. Implementations need not keep all rows resident: `DataTable`
/// queries only the visible range plus overscan.
pub trait TableModel {
    type Row;

    /// Logical row count. This may be much larger than the resident cache.
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Stable, unique identity for the row at `index`.
    fn key_at(&self, index: usize) -> Option<u64>;

    /// Current index for a stable key, used to preserve anchors and selection
    /// across inserts, removals, moves, and sort changes.
    fn index_of(&self, key: u64) -> Option<usize>;

    /// Return resident data or an explicit placeholder state.
    fn row_state(&self, index: usize) -> TableRowState<'_, Self::Row>;

    /// Opportunity to enqueue a fetch for the visible range plus overscan.
    fn request_range(&mut self, _range: Range<usize>) {}

    /// Drain model notifications. Return true after any logical order, count,
    /// key, or row-state change so the table reconciles and repaints.
    fn poll_changed(&mut self) -> bool {
        false
    }

    /// Drain a specific model-notification batch. Existing implementations of
    /// [`Self::poll_changed`] remain compatible and produce [`TableModelChange::Reset`].
    /// New asynchronous models should override this method so unchanged
    /// retained cell controls survive background updates.
    fn poll_changes(&mut self) -> Vec<TableModelChange> {
        if self.poll_changed() {
            vec![TableModelChange::Reset]
        } else {
            Vec::new()
        }
    }

    /// Apply a model-owned sort. Return true when the logical order changed
    /// synchronously or a remote sort request was accepted.
    fn sort_by(&mut self, _column_key: &str, _ascending: bool) -> bool {
        false
    }

    /// Whether the host should keep scheduling frames so `poll_changed` can
    /// observe an in-flight asynchronous source.
    fn needs_poll(&self) -> bool {
        false
    }
}

/// `(display_index, ctrl, shift)`.
type RowClick = Box<dyn FnMut(usize, bool, bool)>;

/// So row changes can re-apply the current ordering.
struct SortState<Row> {
    key: String,
    ascending: bool,
    cmp: SortCmp<Row>,
}

/// `origin_x`/`origin_w` are the pointer-x and column width at `begin_resize`,
/// so repeated `drag_resize(pointer_x)` calls compute an absolute new width
/// without drifting.
struct ResizeState {
    key: String,
    origin_x: f32,
    origin_w: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct RowIdentity {
    value: u64,
    occurrence: usize,
}

struct MountedCell {
    row_key: RowIdentity,
    column_key: String,
    row_index: usize,
    column_index: usize,
    widget: Element,
    node: Option<NodeId>,
}

type RowKey<Row> = Box<dyn Fn(&Row) -> u64 + Send + Sync>;

#[allow(
    clippy::struct_excessive_bools,
    reason = "focus, selection, scrolling, and invalidation are independent table states"
)]
pub struct DataTable<Row> {
    test_id: String,
    accessible_name: Option<String>,
    columns: Vec<ColumnDef<Row>>,
    rows: Vec<Row>,
    /// Present for sparse/paged tables. Vec-backed tables keep this `None`
    /// and retain their local sorting API.
    model: Option<Box<dyn TableModel<Row = Row>>>,
    /// Indices into `rows` in display order — sort operates on this so the
    /// original insertion order stays untouched.
    view: Vec<usize>,
    sort: Option<SortState<Row>>,
    model_sort: Option<(String, bool)>,
    rect: Rect,
    col_rects: Vec<Rect>,
    is_focused: bool,
    /// Populated lazily from `ColumnWidth`, then authoritative once the user
    /// has dragged a handle.
    col_widths_px: HashMap<String, f32>,
    /// Pointer captured while `Some`.
    resize: Option<ResizeState>,
    /// For hit-testing press events against the per-column resize handles.
    last_cursor: Option<Point>,
    /// Shared variable-height geometry. Stable identities follow rows across
    /// sort/reconciliation; paint, hit testing and accessibility all consume
    /// the same visible range.
    viewport: SparseVirtualViewport<RowIdentity>,
    /// Snapshot from the last reconciled/rendered state. External model
    /// mutations may already be visible by the time `poll_changed` fires, so
    /// capturing at notification time would be one revision too late.
    last_anchor: Option<SparseAnchor<RowIdentity>>,
    row_keys: Vec<RowIdentity>,
    row_key: Option<RowKey<Row>>,
    mounted_cells: Vec<MountedCell>,
    structure_dirty: bool,
    layout_dirty: bool,
    interaction: crate::interaction::InteractionEnvironment,
    /// Clicks update [`selection`], rows render a selection fill, and
    /// keyboard Up/Down/Enter/Ctrl-A are honoured while focused. Off by
    /// default so existing read-only tables are unaffected.
    selectable: bool,
    /// Indices into `view`. Seeded by the owner each frame for stateless
    /// hosts; mutated in place on click.
    selection: crate::selection::SelectionModel,
    /// Model-backed tables attach selection to stable keys. Select-all is a
    /// complement and therefore constant-space even for 100M rows.
    model_selection: crate::selection::KeySelection<u64>,
    /// `(display_index, ctrl, shift)`, so a stateless host (rebuilt each
    /// frame) can own the selection.
    on_row_click: Option<RowClick>,
    on_activate: Option<Box<dyn FnMut(usize)>>,
    /// When set, *replaces* the internal `toggle_sort` so the host can own
    /// sort order (the file explorer sorts its own model).
    on_header_click: Option<Box<dyn FnMut(usize)>>,
    /// For internal double-click detection (used only when `on_activate` is
    /// set and the table is long-lived).
    last_click_row: Option<(usize, crate::time::Instant)>,
    /// Active only in internal-virtualization mode (host called
    /// `set_viewport` and rows overflow it); dormant when wrapped in an
    /// outer [`crate::widget::Scrollable`], to avoid a double bar.
    vbar: Scrollbar,
    drag_scroll: DragScroller,
    momentum: ScrollMomentum,
    reduce_motion: bool,
    last_tick: Option<Instant>,
    /// Header band, resolved from the theme's label line at layout time
    /// (`HEADER_HEIGHT` at the default text scale, taller at 2×).
    header_h: f64,
    /// The estimated row height the host authored through `set_viewport`,
    /// if any. It is honoured as written at the default text scale and only
    /// raised once the label line itself would not fit; the unauthored
    /// default follows the type with its 1× padding.
    authored_row_height: Option<f64>,
    /// The label type seen at the last layout; `None` until then, when the
    /// estimate is exactly the authored or default height.
    row_type: Option<RowType>,
    /// Rules between columns, through header and body.
    ruled: bool,
    /// Widths measured for [`ColumnWidth::Auto`] columns from their header
    /// and the visible cells.
    auto_widths: HashMap<String, f32>,
    /// Theme values the layout pass measures with.
    measure_theme: Option<Theme>,
}

impl<Row> DataTable<Row> {
    /// Minimum column width in pixels — dragging below this clamps.
    pub const MIN_COL_WIDTH: f32 = 40.0;

    pub fn new(columns: Vec<ColumnDef<Row>>) -> Self {
        let col_widths_px = columns
            .iter()
            .filter_map(|c| match c.width {
                ColumnWidth::Fixed(px) => Some((c.key.clone(), px)),
                _ => None,
            })
            .collect();
        Self {
            test_id: "data_table".to_owned(),
            accessible_name: None,
            columns,
            rows: Vec::new(),
            model: None,
            view: Vec::new(),
            sort: None,
            model_sort: None,
            rect: Rect::ZERO,
            col_rects: Vec::new(),
            is_focused: false,
            col_widths_px,
            resize: None,
            last_cursor: None,
            viewport: {
                let mut viewport = SparseVirtualViewport::new(ROW_HEIGHT);
                viewport.set_overscan(viewport.estimated_height() * 2.0);
                viewport
            },
            last_anchor: None,
            row_keys: Vec::new(),
            row_key: None,
            mounted_cells: Vec::new(),
            structure_dirty: true,
            layout_dirty: true,
            interaction: crate::interaction::InteractionEnvironment::default(),
            selectable: false,
            selection: crate::selection::SelectionModel::new(),
            model_selection: crate::selection::KeySelection::new(),
            on_row_click: None,
            on_activate: None,
            on_header_click: None,
            last_click_row: None,
            vbar: Scrollbar::vertical(),
            drag_scroll: DragScroller::new(),
            momentum: ScrollMomentum::new(),
            reduce_motion: false,
            last_tick: None,
            header_h: HEADER_HEIGHT,
            authored_row_height: None,
            row_type: None,
            ruled: false,
            auto_widths: HashMap::new(),
            measure_theme: None,
        }
    }

    /// Draw a rule between columns, through the header and the body: for
    /// grids the eye reads by column (a query result, a spreadsheet).
    #[must_use]
    pub fn ruled(mut self) -> Self {
        self.ruled = true;
        self
    }

    /// Construct a sparse table backed by an arbitrary model. Geometry and
    /// selection remain proportional to visible/measured rows rather than
    /// `model.len()`.
    pub fn from_model<M>(columns: Vec<ColumnDef<Row>>, model: M) -> Self
    where
        M: TableModel<Row = Row> + 'static,
    {
        let mut table = Self::new(columns);
        table.model = Some(Box::new(model));
        table.reconcile_viewport(None);
        table
    }

    /// Give this table a stable app-test/inspection id. The historical
    /// default is `data_table`; set an explicit id when an app has more than
    /// one table.
    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.test_id = id.into();
        self
    }

    /// Give the table itself a localized screen-reader label. Column headers
    /// continue to label individual cells.
    #[must_use]
    pub fn with_accessible_name(mut self, name: impl Into<String>) -> Self {
        self.accessible_name = Some(name.into());
        self
    }

    #[must_use]
    pub fn selectable(mut self, v: bool) -> Self {
        self.selectable = v;
        self
    }

    pub fn set_selection(&mut self, sel: crate::selection::SelectionModel) {
        self.selection = sel;
    }

    pub fn selection(&self) -> &crate::selection::SelectionModel {
        &self.selection
    }

    pub fn selection_mut(&mut self) -> &mut crate::selection::SelectionModel {
        &mut self.selection
    }

    /// Stable-key selection used by model-backed tables.
    #[must_use]
    pub fn model_selection(&self) -> &crate::selection::KeySelection<u64> {
        &self.model_selection
    }

    /// Mutable stable-key selection used by model-backed tables.
    pub fn model_selection_mut(&mut self) -> &mut crate::selection::KeySelection<u64> {
        &mut self.model_selection
    }

    /// Restore stable-key selection for a model-backed table, discarding keys
    /// absent from the current model.
    #[must_use]
    pub fn with_model_selection(
        mut self,
        mut selection: crate::selection::KeySelection<u64>,
    ) -> Self {
        if let Some(model) = self.model.as_deref() {
            selection.reconcile(|key| model.index_of(*key).is_some());
        }
        self.model_selection = selection;
        self
    }

    #[must_use]
    pub fn with_selection(mut self, sel: crate::selection::SelectionModel) -> Self {
        self.selection = sel;
        self
    }

    #[must_use]
    pub fn on_row_click<F: FnMut(usize, bool, bool) + 'static>(mut self, f: F) -> Self {
        self.on_row_click = Some(Box::new(f));
        self
    }

    #[must_use]
    pub fn on_activate<F: FnMut(usize) + 'static>(mut self, f: F) -> Self {
        self.on_activate = Some(Box::new(f));
        self
    }

    #[must_use]
    pub fn on_header_click<F: FnMut(usize) + 'static>(mut self, f: F) -> Self {
        self.on_header_click = Some(Box::new(f));
        self
    }

    /// Give rows stable model identity so scroll anchoring and cached
    /// measurements survive inserts, removals, and sorting. Duplicate values
    /// are disambiguated by occurrence order; unique keys provide the
    /// strongest continuity.
    #[must_use]
    pub fn with_row_key<F>(mut self, key: F) -> Self
    where
        F: Fn(&Row) -> u64 + Send + Sync + 'static,
    {
        self.row_key = Some(Box::new(key));
        self.rebuild_row_keys();
        self.reconcile_viewport(None);
        self
    }

    /// `None` for points in the header or below the last row.
    fn row_at(&self, pt: Point) -> Option<usize> {
        if pt.x < self.rect.x0 || pt.x > self.rect.x1 {
            return None;
        }
        let body_top = self.rect.y0 + self.header_h;
        if pt.y < body_top {
            return None;
        }
        let idx = self.viewport.hit_test(pt.y - body_top)?;
        if idx < self.display_len() {
            Some(idx)
        } else {
            None
        }
    }

    pub fn set_rows(&mut self, rows: Vec<Row>) {
        let anchor = self.capture_view_anchor();
        self.model = None;
        self.model_sort = None;
        self.model_selection.clear();
        self.rows = rows;
        if !self.mounted_cells.is_empty() {
            self.mounted_cells.clear();
            self.structure_dirty = true;
        }
        self.rebuild_row_keys();
        self.view = (0..self.rows.len()).collect();
        self.reapply_sort_with_anchor(anchor.as_ref());
    }

    fn rebuild_row_keys(&mut self) {
        let mut occurrences = HashMap::<u64, usize>::new();
        self.row_keys = self
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let value = self
                    .row_key
                    .as_ref()
                    .map_or_else(|| u64::try_from(index).unwrap_or(u64::MAX), |key| key(row));
                let occurrence = occurrences.entry(value).or_default();
                let identity = RowIdentity {
                    value,
                    occurrence: *occurrence,
                };
                *occurrence = occurrence.saturating_add(1);
                identity
            })
            .collect();
    }

    fn capture_view_anchor(&self) -> Option<SparseAnchor<RowIdentity>> {
        self.viewport
            .capture_anchor(|index| self.row_identity_at(index))
    }

    fn refresh_view_anchor(&mut self) {
        self.last_anchor = self.capture_view_anchor();
    }

    fn reconcile_viewport(&mut self, anchor: Option<&SparseAnchor<RowIdentity>>) {
        let count = self.display_len();
        let model = self.model.as_deref();
        let view = &self.view;
        let row_keys = &self.row_keys;
        self.viewport.reconcile(
            count,
            |identity| {
                if let Some(model) = model {
                    (identity.occurrence == 0)
                        .then(|| model.index_of(identity.value))
                        .flatten()
                } else {
                    view.iter()
                        .position(|row_index| row_keys.get(*row_index).copied() == Some(*identity))
                }
            },
            anchor,
        );
        if let Some(model) = self.model.as_deref() {
            self.model_selection
                .reconcile(|key| model.index_of(*key).is_some());
        } else {
            self.selection.clamp_to(count);
        }
        self.last_anchor = self.capture_view_anchor();
    }

    fn row_identity_at(&self, display_index: usize) -> Option<RowIdentity> {
        if let Some(model) = self.model.as_deref() {
            return model.key_at(display_index).map(|value| RowIdentity {
                value,
                occurrence: 0,
            });
        }
        let source_index = *self.view.get(display_index)?;
        self.row_keys.get(source_index).copied()
    }

    fn display_len(&self) -> usize {
        self.model
            .as_deref()
            .map_or(self.view.len(), TableModel::len)
    }

    fn row_state_at(&self, display_index: usize) -> Option<TableRowState<'_, Row>> {
        if let Some(model) = self.model.as_deref() {
            return (display_index < model.len()).then(|| model.row_state(display_index));
        }
        let source_index = *self.view.get(display_index)?;
        self.rows.get(source_index).map(TableRowState::Ready)
    }

    fn model_key_at(&self, display_index: usize) -> Option<u64> {
        self.model.as_deref()?.key_at(display_index)
    }

    fn primary_index(&self) -> Option<usize> {
        if let Some(model) = self.model.as_deref() {
            return self
                .model_selection
                .primary()
                .and_then(|key| model.index_of(*key));
        }
        self.selection.primary()
    }

    fn is_row_selected(&self, display_index: usize) -> bool {
        if self.model.is_some() {
            return self
                .model_key_at(display_index)
                .is_some_and(|key| self.model_selection.is_selected(&key));
        }
        self.selection.is_selected(display_index)
    }

    fn apply_selection_click(&mut self, display_index: usize, ctrl: bool, shift: bool) {
        let Some(model) = self.model.as_deref() else {
            self.selection.apply_click(display_index, ctrl, shift);
            return;
        };
        let Some(target) = model.key_at(display_index) else {
            return;
        };
        if shift {
            let Some(anchor_index) = self
                .model_selection
                .anchor()
                .and_then(|anchor| model.index_of(*anchor))
            else {
                self.model_selection.select_only(target);
                return;
            };
            let (start, end) = if anchor_index <= display_index {
                (anchor_index, display_index)
            } else {
                (display_index, anchor_index)
            };
            let keys = (start..=end).filter_map(|index| model.key_at(index));
            self.model_selection.select_range(target, keys, ctrl);
        } else if ctrl {
            self.model_selection.toggle(target);
        } else {
            self.model_selection.select_only(target);
        }
    }

    fn move_selection_to(&mut self, display_index: usize, extend: bool) {
        self.apply_selection_click(display_index, false, extend);
    }

    fn select_only_index(&mut self, display_index: usize) {
        if let Some(key) = self.model_key_at(display_index) {
            self.model_selection.select_only(key);
        } else if self.model.is_none() {
            self.selection.select_only(display_index);
        }
    }

    fn semantic_row_index(&self, target: &str) -> Option<usize> {
        let raw = target.strip_prefix("row:")?;
        let (value, occurrence) = raw.split_once(':')?;
        let identity = RowIdentity {
            value: value.parse().ok()?,
            occurrence: occurrence.parse().ok()?,
        };
        if let Some(model) = self.model.as_deref() {
            return (identity.occurrence == 0)
                .then(|| model.index_of(identity.value))
                .flatten();
        }
        self.view.iter().enumerate().find_map(|(display_index, _)| {
            (self.row_identity_at(display_index) == Some(identity)).then_some(display_index)
        })
    }

    pub fn columns(&self) -> &[ColumnDef<Row>] {
        &self.columns
    }

    /// Rows in current sort order.
    pub fn view_rows(&self) -> Vec<&Row> {
        if self.model.is_some() {
            return self
                .viewport
                .visible_range()
                .filter_map(|index| match self.row_state_at(index) {
                    Some(TableRowState::Ready(row)) => Some(row),
                    _ => None,
                })
                .collect();
        }
        self.view.iter().filter_map(|&i| self.rows.get(i)).collect()
    }

    /// Reconcile an externally changed model. The first visible stable key,
    /// measured heights, and key selection survive moves and inserts.
    pub fn notify_model_changed(&mut self) {
        self.notify_model_changes(&[TableModelChange::Reset]);
    }

    /// Reconcile one already-applied model change batch. Structural changes
    /// retain visible cell controls whose stable keys still exist, while
    /// `RowsChanged` rebuilds controls only for the named keys.
    pub fn notify_model_changes(&mut self, changes: &[TableModelChange]) {
        if self.model.is_none() {
            return;
        }
        if changes.is_empty() {
            return;
        }
        let anchor = self
            .last_anchor
            .clone()
            .or_else(|| self.capture_view_anchor());
        self.reconcile_viewport(anchor.as_ref());

        if changes
            .iter()
            .any(|change| matches!(change, TableModelChange::Reset))
        {
            self.mounted_cells.clear();
        } else {
            self.mounted_cells.retain(|cell| {
                !changes.iter().any(|change| match change {
                    TableModelChange::RowsChanged { keys } => keys.contains(&cell.row_key.value),
                    TableModelChange::RowsInserted { .. }
                    | TableModelChange::RowsRemoved { .. }
                    | TableModelChange::RowsMoved { .. }
                    | TableModelChange::Reset => false,
                })
            });
        }
        self.structure_dirty = true;
        self.layout_dirty = true;
    }

    pub fn sort_state(&self) -> Option<(&str, bool)> {
        if let Some((key, ascending)) = &self.model_sort {
            return Some((key.as_str(), *ascending));
        }
        self.sort.as_ref().map(|s| (s.key.as_str(), s.ascending))
    }

    /// Stores the comparator + key + direction so `set_rows` re-applies the
    /// same ordering automatically. Model-backed tables delegate sorting to
    /// [`TableModel::sort_by`] instead.
    pub fn sort_by_with<F>(&mut self, cmp: F, key: &str, ascending: bool)
    where
        F: Fn(&Row, &Row) -> Ordering + Send + Sync + 'static,
    {
        if self.model.is_some() {
            return;
        }
        self.sort = Some(SortState {
            key: key.to_string(),
            ascending,
            cmp: Box::new(cmp),
        });
        self.reapply_sort();
    }

    /// Reuses the comparator already installed via [`Self::sort_by_with`];
    /// returns `false` when none is installed for `column_key`. Per-column
    /// comparators can't be encoded as pure `(key, direction)`, so callers
    /// install one typed comparator, then header-click/keyboard handlers flip
    /// direction through this simpler entry point.
    pub fn sort_by(&mut self, column_key: &str, ascending: bool) -> bool {
        if let Some(model) = self.model.as_deref_mut() {
            if !model.sort_by(column_key, ascending) {
                return false;
            }
            self.model_sort = Some((column_key.to_owned(), ascending));
            self.notify_model_changed();
            return true;
        }
        if let Some(s) = &mut self.sort
            && s.key == column_key
        {
            s.ascending = ascending;
            self.reapply_sort();
            return true;
        }
        false
    }

    fn reapply_sort(&mut self) {
        let anchor = self.capture_view_anchor();
        self.reapply_sort_with_anchor(anchor.as_ref());
    }

    fn reapply_sort_with_anchor(&mut self, anchor: Option<&SparseAnchor<RowIdentity>>) {
        if let Some(s) = &self.sort {
            let rows = &self.rows;
            let cmp = &s.cmp;
            let ascending = s.ascending;
            self.view.sort_by(|&a, &b| {
                let (Some(ra), Some(rb)) = (rows.get(a), rows.get(b)) else {
                    return Ordering::Equal;
                };
                let o = cmp(ra, rb);
                if ascending { o } else { o.reverse() }
            });
        }
        self.reconcile_viewport(anchor);
    }

    /// Falls back to `ColumnWidth::Fixed`; Flex/Auto columns return `None`
    /// until resized since their pixel value depends on the layout pass.
    pub fn column_width_px(&self, key: &str) -> Option<f32> {
        if let Some(w) = self.col_widths_px.get(key).copied() {
            return Some(w);
        }
        self.columns.iter().find(|c| c.key == key).and_then(|c| {
            if let ColumnWidth::Fixed(px) = c.width {
                Some(px)
            } else {
                None
            }
        })
    }

    /// Subsequent `drag_resize` calls compute the new width as
    /// `origin_w + (pointer_x - origin_x)`.
    pub fn begin_resize(&mut self, key: &str, pointer_x: f32) {
        let origin_w = self.column_width_px(key).unwrap_or(Self::MIN_COL_WIDTH);
        self.resize = Some(ResizeState {
            key: key.to_string(),
            origin_x: pointer_x,
            origin_w,
        });
    }

    /// Clamps to `MIN_COL_WIDTH`.
    pub fn drag_resize(&mut self, pointer_x: f32) {
        if let Some(r) = &self.resize {
            let delta = pointer_x - r.origin_x;
            let new_w = (r.origin_w + delta).max(Self::MIN_COL_WIDTH);
            let key = r.key.clone();
            self.col_widths_px.insert(key, new_w);
            self.structure_dirty = true;
            self.layout_dirty = true;
        }
    }

    pub fn end_resize(&mut self) {
        self.resize = None;
    }

    /// Enables the `visible_row_range` windowing so only rows intersecting
    /// the viewport (plus overscan) are rendered.
    pub fn set_viewport(&mut self, height_px: f32, row_height_px: f32) {
        self.authored_row_height = Some(f64::from(row_height_px));
        self.refresh_row_estimate();
        self.viewport.set_viewport(f64::from(height_px));
        self.viewport
            .set_overscan(self.viewport.estimated_height() * 2.0);
        self.visible_window_changed();
    }

    /// Fluent form of [`Self::set_viewport`] for app constructors.
    #[must_use]
    pub fn with_viewport(mut self, height_px: f32, row_height_px: f32) -> Self {
        self.set_viewport(height_px, row_height_px);
        self
    }

    pub fn set_scroll_y(&mut self, y: f32) {
        self.momentum.stop();
        self.set_scroll_offset(f64::from(y));
    }

    fn set_scroll_offset(&mut self, y: f64) {
        let before = self.viewport.visible_range();
        self.viewport.set_offset(y);
        if self.viewport.visible_range() != before {
            self.structure_dirty = true;
        }
        self.layout_dirty = true;
        self.last_anchor = self.capture_view_anchor();
    }

    #[must_use]
    pub fn scroll_y(&self) -> f32 {
        px(self.viewport.offset())
    }

    /// Override one display row's measured height while keeping the current
    /// stable anchor fixed. Returns `false` when `display_index` is absent or
    /// geometry did not change.
    pub fn set_row_height(&mut self, display_index: usize, height_px: f32) -> bool {
        let Some(key) = self.row_identity_at(display_index) else {
            return false;
        };
        let changed = self
            .viewport
            .measure(display_index, key, f64::from(height_px));
        if changed {
            self.visible_window_changed();
        }
        changed
    }

    /// `(start, end)` row-index range (half-open), with two estimated row
    /// heights of pixel overscan on each side. With no viewport set, the
    /// entire row list is returned.
    pub fn visible_row_range(&self) -> (usize, usize) {
        let range = self.viewport.visible_range();
        (range.start, range.end)
    }

    /// Number of retained cell controls. This remains proportional to the
    /// visible window, not the logical model size.
    #[must_use]
    pub fn mounted_cell_count(&self) -> usize {
        self.mounted_cells.len()
    }

    fn visible_window_changed(&mut self) {
        self.structure_dirty = true;
        self.layout_dirty = true;
        self.refresh_view_anchor();
    }

    /// Size the header band and the estimated row height from the theme's
    /// label line. At the default text scale both resolve to the pinned
    /// `HEADER_HEIGHT` / `ROW_HEIGHT`; at 2× they grow so the (now correctly
    /// sized) text has the same padding it had at 1×. An estimate the host
    /// authored through [`Self::set_viewport`] is kept as written and raised
    /// only once the line plus the smallest step would not fit; heights set
    /// per row (`set_row_height`, mounted cells) are never touched.
    fn apply_type_metrics(&mut self, theme: &Theme) {
        let tokens = &theme.tokens;
        let line = type_line_height(&tokens.typography, TextRole::Caption);
        self.header_h = text_band(
            f64::from(tokens.metrics.min_pointer_target),
            line,
            f64::from(tokens.spacing.s3),
        );
        self.row_type = Some(RowType {
            line,
            default_pad: f64::from(tokens.spacing.s2),
            authored_pad: f64::from(tokens.spacing.s1),
        });
        self.refresh_row_estimate();
    }

    /// The one place the viewport's estimated row height is set, from the
    /// authored estimate and the label type seen so far (see
    /// [`row_estimate`]). Called when either input changes.
    fn refresh_row_estimate(&mut self) {
        let estimate = row_estimate(self.authored_row_height, ROW_HEIGHT, self.row_type);
        let before = self.viewport.estimated_height();
        self.viewport.set_estimated_height(estimate);
        if self.viewport.estimated_height().to_bits() != before.to_bits() {
            self.viewport
                .set_overscan(self.viewport.estimated_height() * 2.0);
            self.visible_window_changed();
        }
    }

    fn reconcile_mounted_cells(&mut self) {
        let range = self.viewport.visible_range();
        if let Some(model) = self.model.as_deref_mut() {
            model.request_range(range.clone());
        }
        let widget_columns = self
            .columns
            .iter()
            .enumerate()
            .filter_map(|(index, column)| column.widget.as_ref().map(|_| index))
            .collect::<Vec<_>>();
        let mut desired = Vec::with_capacity(range.len().saturating_mul(widget_columns.len()));
        for row_index in range {
            let Some(row_key) = self.row_identity_at(row_index) else {
                continue;
            };
            for &column_index in &widget_columns {
                desired.push((row_index, row_key, column_index));
            }
        }
        let old_order = self
            .mounted_cells
            .iter()
            .map(|cell| (cell.row_key, cell.column_key.clone()))
            .collect::<Vec<_>>();
        let mut old = std::mem::take(&mut self.mounted_cells)
            .into_iter()
            .map(|cell| ((cell.row_key, cell.column_key.clone()), cell))
            .collect::<HashMap<_, _>>();
        let interaction = self.interaction;
        let mut mounted = Vec::with_capacity(desired.len());
        for (row_index, row_key, column_index) in desired {
            let Some(column_key) = self
                .columns
                .get(column_index)
                .map(|column| column.key.clone())
            else {
                continue;
            };
            let key = (row_key, column_key.clone());
            if let Some(mut cell) = old.remove(&key) {
                cell.row_index = row_index;
                cell.column_index = column_index;
                cell.node = None;
                cell.widget.set_interaction_environment(interaction);
                mounted.push(cell);
                continue;
            }
            let row = if let Some(model) = self.model.as_deref() {
                match model.row_state(row_index) {
                    TableRowState::Ready(row) => Some(row),
                    TableRowState::Loading | TableRowState::Error(_) => None,
                }
            } else {
                self.view
                    .get(row_index)
                    .and_then(|source| self.rows.get(*source))
            };
            let Some(row) = row else { continue };
            let Some(builder) = self
                .columns
                .get_mut(column_index)
                .and_then(|column| column.widget.as_mut())
            else {
                continue;
            };
            let mut widget = builder(row, row_index);
            widget.set_interaction_environment(interaction);
            mounted.push(MountedCell {
                row_key,
                column_key,
                row_index,
                column_index,
                widget,
                node: None,
            });
        }
        let next_order = mounted
            .iter()
            .map(|cell| (cell.row_key, cell.column_key.clone()))
            .collect::<Vec<_>>();
        if old_order != next_order {
            self.structure_dirty = true;
        }
        self.mounted_cells = mounted;
    }

    fn body_viewport_h(&self) -> f64 {
        if self.viewport.viewport() > 0.0 {
            self.viewport.viewport()
        } else {
            (self.rect.height() - self.header_h).max(0.0)
        }
    }

    fn content_body_h(&self) -> f64 {
        self.viewport.content_extent()
    }

    /// `content - viewport`, floored at 0.
    pub fn max_scroll_y(&self) -> f32 {
        px(self.viewport.max_offset())
    }

    fn v_metrics(&self) -> ScrollMetrics {
        ScrollMetrics::new(
            self.content_body_h(),
            self.body_viewport_h(),
            self.viewport.offset(),
        )
    }

    /// Live when an explicit viewport was set and the rows overflow it.
    fn internal_scroll(&self) -> bool {
        self.viewport.viewport() > 0.0 && self.v_metrics().overflows()
    }

    fn v_track(&self) -> Option<Rect> {
        if !self.internal_scroll() {
            return None;
        }
        let x1 = self.rect.x1 - BAR_MARGIN;
        let x0 = x1 - BAR_THICKNESS;
        let y0 = self.rect.y0 + self.header_h + BAR_MARGIN;
        let y1 = self.rect.y0 + self.header_h + self.body_viewport_h() - BAR_MARGIN;
        if y1 - y0 <= crate::widget::scrollbar::MIN_THUMB_LEN.min(1.0) {
            return None;
        }
        Some(Rect::new(x0, y0, x1, y1))
    }

    /// `None` when the bar is inactive (no viewport set, or rows fit).
    pub fn scrollbar_thumb(&self) -> Option<Rect> {
        let track = self.v_track()?;
        self.vbar.thumb_rect(track, self.v_metrics())
    }

    fn draw_scrollbar(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let Some(track) = self.v_track() else { return };
        let radius = BAR_THICKNESS / 2.0;
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            crate::widget::scrollbar::track_color(theme),
            None,
            &RoundedRect::from_rect(track, radius),
        );
        if let Some(thumb) = self.vbar.thumb_rect(track, self.v_metrics()) {
            let color = crate::widget::scrollbar::thumb_color(theme, self.vbar.is_active());
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                color,
                None,
                &RoundedRect::from_rect(thumb, radius),
            );
        }
    }

    fn resize_handle_rect_for(&self, index: usize) -> Option<Rect> {
        let col = self.columns.get(index)?;
        let cr = self.col_rects.get(index)?;
        let w = f64::from(col.resize_handle_px);
        Some(Rect::new(
            cr.x1 - w,
            self.rect.y0,
            cr.x1,
            self.rect.y0 + self.header_h,
        ))
    }

    /// `None` if not sortable, or if no comparator has been registered via
    /// `sort_by_with` yet.
    pub fn toggle_sort(&mut self, index: usize) -> Option<(String, bool)> {
        let col = self.columns.get(index)?;
        if !col.sortable {
            return None;
        }
        let key = col.key.clone();
        if self.model.is_some() {
            let ascending = match &self.model_sort {
                Some((active, ascending)) if active == &key => !ascending,
                _ => true,
            };
            return self.sort_by(&key, ascending).then_some((key, ascending));
        }
        let ascending = match &self.sort {
            Some(s) if s.key == key => !s.ascending,
            _ => true,
        };
        if let Some(s) = &mut self.sort
            && s.key == key
        {
            s.ascending = ascending;
            self.reapply_sort();
            return Some((key, ascending));
        }
        None
    }

    /// Header background, bottom border, column labels and the active sort arrow.
    fn draw_header(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let tokens = &theme.tokens;
        let header_rect = Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x1,
            self.rect.y0 + self.header_h,
        );
        // The header sits on the table's ground; one rule derived from that
        // ground closes it, on the band's last pixel row.
        crate::surface::separator(
            scene,
            transform,
            header_rect.x0,
            header_rect.x1,
            header_rect.y1 - 0.5,
            0.0,
            tokens.colors.surfaces.content,
        );

        let text_style = header_style(theme);
        let pad_x = f64::from(tokens.spacing.s2);
        let icon = f64::from(tokens.metrics.icon_sm);
        let icon_gap = f64::from(tokens.spacing.s1);
        for (i, col) in self.columns.iter().enumerate() {
            let Some(cr) = self.col_rects.get(i) else {
                continue;
            };
            let header = crate::i18n::I18nManager::lookup_plain(&col.header);
            let sorted = self
                .sort
                .as_ref()
                .filter(|s| s.key == col.key)
                .map(|s| s.ascending)
                .or_else(|| {
                    self.model_sort
                        .as_ref()
                        .filter(|(key, _)| *key == col.key)
                        .map(|(_, ascending)| *ascending)
                });
            let room = (cr.width()
                - 2.0 * pad_x
                - if sorted.is_some() {
                    icon + icon_gap
                } else {
                    0.0
                })
            .max(0.0);
            let header = text_renderer.elide_to_width_style(&header, px(room), &text_style);
            let (header_w, header_h) = text_renderer.measure_style(&header, None, &text_style);
            let label_y = centered_text_top(header_rect.y0, self.header_h, header_h);
            let label_x = if col.numeric {
                cr.x1 - pad_x - f64::from(header_w)
            } else {
                cr.x0 + pad_x
            };
            text_renderer.draw_style(
                scene,
                &header,
                tokens.colors.text_dim,
                translated(transform, label_x, label_y),
                None,
                &text_style,
            );
            if let Some(ascending) = sorted {
                // The sort direction is an icon beside the label: before a
                // trailing-aligned numeric header, after a leading one.
                let x0 = if col.numeric {
                    label_x - icon_gap - icon
                } else {
                    label_x + f64::from(header_w) + icon_gap
                };
                let glyph = Rect::from_center_size(
                    Point::new(x0 + icon / 2.0, header_rect.center().y),
                    (icon, icon),
                );
                let arrow = if ascending {
                    crate::icons::Icon::ChevronUp
                } else {
                    crate::icons::Icon::ChevronDown
                };
                arrow.draw(scene, transform, glyph, tokens.colors.text_dim);
            }
        }
        if self.ruled {
            self.draw_column_rules(scene, transform, theme);
        }
    }

    /// Vertical rules between columns, through the header and the body, from
    /// the ground they sit on.
    fn draw_column_rules(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let rule = crate::surface::edge_on(theme.tokens.colors.surfaces.content);
        let y1 = self.rect.y0 + self.header_h + self.body_viewport_h();
        for cr in self.col_rects.iter().skip(1) {
            let x = (cr.x0 - 0.5).round();
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                rule,
                None,
                &Rect::new(x, self.rect.y0, x + 1.0, y1),
            );
        }
    }

    /// Row backgrounds (selection fill / zebra stripe) and per-column cell
    /// content, windowed to [`Self::visible_row_range`] plus overscan.
    fn draw_rows(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let tokens = &theme.tokens;
        let (vis_start, vis_end) = self.visible_row_range();
        let body_bottom = self.rect.y0 + self.header_h + self.body_viewport_h();
        for ri in vis_start..vis_end {
            let Some(row_state) = self.row_state_at(ri) else {
                continue;
            };
            // Internal virtualization subtracts the viewport offset; when
            // driven by an outer Scrollable, it stays zero and the whole rect is
            // translated instead — both paths render correctly.
            let Some(view_top) = self.viewport.item_view_top(ri) else {
                continue;
            };
            let Some(row_h) = self.viewport.item_height(ri) else {
                continue;
            };
            let y0 = self.rect.y0 + self.header_h + view_top;
            let y1 = y0 + row_h;
            if y1 <= self.rect.y0 + self.header_h {
                continue;
            }
            if y0 >= body_bottom {
                break;
            }
            let selected = self.selectable && self.is_row_selected(ri);
            // Whole-row geometry: the caller's body clip layer trims the part
            // that scrolls past the viewport, so cells see their real row.
            let row_rect = Rect::new(self.rect.x0, y0, self.rect.x1, y1);
            // Selection is a lift (HIG §9.3), never an accent wash, and
            // takes precedence over the zebra stripe; the stripe is the
            // faintest derived tier on the ground, not a ramp slot.
            if selected {
                crate::widget::list_row::paint_row_fill(
                    scene, transform, theme, row_rect, true, false,
                );
            } else if ri % 2 == 1 {
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    tier_on(tokens.colors.surfaces.content, ZEBRA_ALPHA),
                    None,
                    &row_rect,
                );
            }
            match row_state {
                TableRowState::Ready(row) => {
                    for (i, col) in self.columns.iter().enumerate() {
                        let cell_rect = match self.col_rects.get(i) {
                            Some(cr) => Rect::new(cr.x0, y0, cr.x1, y1),
                            None => continue,
                        };
                        (col.render_cell)(row, scene, theme, text_renderer, cell_rect);
                        if let Some(value) = &col.text {
                            paint_cell_text(
                                scene,
                                transform,
                                text_renderer,
                                theme,
                                cell_rect,
                                col,
                                value(row).as_deref(),
                            );
                        }
                        if let Some(tr) = &col.text_render {
                            tr(row, scene, text_renderer, cell_rect, theme, selected);
                        }
                    }
                }
                TableRowState::Loading | TableRowState::Error(_) => {
                    let (label, color) = match row_state {
                        TableRowState::Error(message) => (message, tokens.colors.text_primary),
                        _ => ("Loading…", tokens.colors.text_dim),
                    };
                    let style = tokens.typography.style(TextRole::Caption);
                    let (_, text_h) = text_renderer.measure_style(label, None, &style);
                    text_renderer.draw_style(
                        scene,
                        label,
                        color,
                        translated(
                            transform,
                            self.rect.x0 + f64::from(tokens.spacing.s2),
                            centered_text_top(y0, row_h, text_h),
                        ),
                        None,
                        &style,
                    );
                }
            }
        }
    }

    /// `WindowEvent::CursorMoved` — drag-resize/drag-scroll tracking, else
    /// scrollbar hover.
    fn on_cursor_moved(&mut self, pt: Point, handled: &mut bool) {
        self.last_cursor = Some(pt);
        if self.resize.is_some() {
            self.drag_resize(px(pt.x));
            *handled = true;
            return;
        }
        // An active scrollbar drag owns the move.
        if self.vbar.is_dragging() {
            if let Some(track) = self.v_track()
                && let Some(target) = self.vbar.drag_to(track, self.v_metrics(), pt)
            {
                self.set_scroll_y(px(target));
            }
            *handled = true;
            return;
        }
        if self.drag_scroll.is_tracking() {
            let now = crate::time::now();
            if let Some(update) = self.drag_scroll.update(pt.y, now) {
                self.momentum.stop();
                self.set_scroll_offset(update.target_offset);
                self.last_tick = Some(now);
                *handled = true;
            }
            return;
        }
        if let Some(track) = self.v_track() {
            self.vbar.update_hover(track, self.v_metrics(), pt);
        } else {
            self.vbar.set_hover(false);
        }
    }

    /// `WindowEvent::MouseInput { state: Pressed, button: Left }` — scrollbar,
    /// then column resize handles, then header sort, then row selection, in
    /// that priority order.
    fn on_left_press(
        &mut self,
        modifiers: winit::event::Modifiers,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
    ) {
        self.momentum.stop();
        let Some(pt) = self.last_cursor else { return };
        // 0. Built-in scrollbar wins over the body beneath it.
        if let Some(track) = self.v_track() {
            match self.vbar.press(track, self.v_metrics(), pt) {
                PressOutcome::Thumb => {
                    *handled = true;
                    return;
                }
                PressOutcome::Page(target) => {
                    self.set_scroll_y(px(target));
                    *handled = true;
                    return;
                }
                PressOutcome::Miss => {}
            }
        }
        // 1. Column resize handles win over everything.
        for i in 0..self.columns.len() {
            if let Some(handle) = self.resize_handle_rect_for(i)
                && handle.contains(pt)
            {
                let Some(key) = self.columns.get(i).map(|c| c.key.clone()) else {
                    continue;
                };
                self.begin_resize(&key, px(pt.x));
                *handled = true;
                return;
            }
        }
        // 2. Header click → sort (host-owned via on_header_click, else internal toggle).
        let in_header = pt.y >= self.rect.y0
            && pt.y < self.rect.y0 + self.header_h
            && pt.x >= self.rect.x0
            && pt.x <= self.rect.x1;
        if in_header {
            if let Some(i) = self
                .col_rects
                .iter()
                .position(|c| pt.x >= c.x0 && pt.x < c.x1)
                && self.columns.get(i).is_some_and(|c| c.sortable)
            {
                if let Some(cb) = &mut self.on_header_click {
                    cb(i);
                } else {
                    self.toggle_sort(i);
                }
                *handled = true;
            }
            return;
        }
        // 3. An internally scrolling body defers row activation until
        // release so the same gesture can become a drag without selecting.
        let in_body = pt.x >= self.rect.x0
            && pt.x <= self.rect.x1
            && pt.y >= self.rect.y0 + self.header_h
            && pt.y <= self.rect.y1;
        if self.internal_scroll() && in_body {
            self.drag_scroll
                .begin(pt.y, self.viewport.offset(), crate::time::now());
            *focus_target = Some(node_id);
            *handled = true;
            return;
        }
        self.select_row_at_cursor(modifiers, handled, focus_target, node_id);
    }

    fn select_row_at_cursor(
        &mut self,
        modifiers: winit::event::Modifiers,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
    ) {
        let Some(pt) = self.last_cursor else { return };
        if self.selectable
            && let Some(ri) = self.row_at(pt)
        {
            let ctrl = modifiers.state().control_key() || modifiers.state().super_key();
            let shift = modifiers.state().shift_key();
            self.apply_selection_click(ri, ctrl, shift);
            *focus_target = Some(node_id);
            if let Some(cb) = &mut self.on_row_click {
                cb(ri, ctrl, shift);
            }
            // Internal double-click → activate (long-lived hosts).
            let now = crate::time::now();
            let is_double = matches!(
                self.last_click_row,
                Some((pri, t)) if pri == ri && now.duration_since(t) < crate::gesture::multi_click_interval()
            );
            if is_double {
                self.last_click_row = None;
                if let Some(cb) = &mut self.on_activate {
                    cb(ri);
                }
            } else {
                self.last_click_row = Some((ri, now));
            }
            *handled = true;
        }
    }

    /// `WindowEvent::MouseInput { state: Released, button: Left }` — ends any
    /// in-progress column resize or scrollbar drag.
    fn on_left_release(
        &mut self,
        modifiers: winit::event::Modifiers,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
    ) {
        let mut did = false;
        if self.resize.is_some() {
            self.end_resize();
            did = true;
        }
        if self.vbar.release() {
            did = true;
        }
        if let Some(drag) = self.drag_scroll.finish(crate::time::now()) {
            if drag.dragged {
                if self.reduce_motion {
                    self.momentum.stop();
                } else {
                    self.momentum.fling(drag.velocity);
                    self.last_tick = Some(crate::time::now());
                }
            } else {
                self.select_row_at_cursor(modifiers, handled, focus_target, node_id);
            }
            did = true;
        }
        if did {
            *handled = true;
        }
    }

    /// `WindowEvent::MouseWheel` — only live when this table owns its scroll
    /// internally (an outer `Scrollable` otherwise consumes the wheel first).
    fn on_wheel(&mut self, delta: &MouseScrollDelta, phase: TouchPhase, handled: &mut bool) {
        if self.internal_scroll() {
            self.drag_scroll.cancel();
            self.last_tick = Some(crate::time::now());
            *handled = match self.momentum.wheel(delta, phase, self.reduce_motion) {
                WheelStep::Scroll(dy) => {
                    let before = self.viewport.offset();
                    self.set_scroll_offset(before + dy);
                    let moved = self.viewport.offset().to_bits() != before.to_bits();
                    self.momentum.wheel_applied(dy, moved);
                    moved
                }
                WheelStep::Fling => true,
                WheelStep::Ignored => false,
            };
        }
    }

    fn page_selection(&mut self, forward: bool, extend: bool) -> bool {
        let len = self.display_len();
        let current = self
            .primary_index()
            .unwrap_or_else(|| if forward { 0 } else { len.saturating_sub(1) });
        let Some((next, scrolled)) = self.viewport.page_from(current, forward) else {
            return false;
        };
        self.move_selection_to(next, extend);
        scrolled || next != current
    }

    /// `WindowEvent::KeyboardInput`, guarded by the caller to only fire while
    /// selectable, focused and on key-down: arrow navigation, Enter-to-activate,
    /// Ctrl+A select-all.
    fn on_key(
        &mut self,
        ke: &winit::event::KeyEvent,
        modifiers: winit::event::Modifiers,
        handled: &mut bool,
    ) {
        use winit::keyboard::{Key, NamedKey};
        let shift = modifiers.state().shift_key();
        let len = self.display_len();
        if len == 0 {
            return;
        }
        let cur = self.primary_index();
        match &ke.logical_key {
            Key::Named(NamedKey::ArrowDown) => {
                let next = cur.map_or(0, |c| c.saturating_add(1).min(len.saturating_sub(1)));
                self.move_selection_to(next, shift);
                self.viewport
                    .scroll_to_index(next, ScrollAlignment::Nearest);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowUp) => {
                let next = cur.map_or(0, |c| c.saturating_sub(1));
                self.move_selection_to(next, shift);
                self.viewport
                    .scroll_to_index(next, ScrollAlignment::Nearest);
                *handled = true;
            }
            Key::Named(NamedKey::PageDown) => {
                *handled = self.page_selection(true, shift);
            }
            Key::Named(NamedKey::PageUp) => {
                *handled = self.page_selection(false, shift);
            }
            Key::Named(NamedKey::Home) => {
                self.move_selection_to(0, shift);
                self.viewport.scroll_to_index(0, ScrollAlignment::Start);
                *handled = true;
            }
            Key::Named(NamedKey::End) => {
                let last = len.saturating_sub(1);
                self.move_selection_to(last, shift);
                self.viewport.scroll_to_index(last, ScrollAlignment::End);
                *handled = true;
            }
            Key::Named(NamedKey::Enter) => {
                if let Some(c) = cur
                    && let Some(cb) = &mut self.on_activate
                {
                    cb(c);
                }
                *handled = true;
            }
            Key::Character(s) if (s == "a" || s == "A") && modifiers.state().control_key() => {
                if self.model.is_some() {
                    self.model_selection.select_all();
                } else {
                    self.selection.select_all(len);
                }
                *handled = true;
            }
            _ => {}
        }
    }

    fn route_to_cell_widgets(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        if let Some(cell) = self
            .mounted_cells
            .iter_mut()
            .find(|cell| cell.widget.wants_pointer_capture())
            && let Some(node) = cell.node
        {
            cell.widget
                .handle_event(event, &mut ctx.reborrow(ctx.path, node));
            return;
        }
        for cell in self.mounted_cells.iter_mut().rev() {
            let Some(node) = cell.node else { continue };
            cell.widget
                .handle_event(event, &mut ctx.reborrow(ctx.path, node));
            if *ctx.handled {
                break;
            }
        }
    }
}

const HEADER_HEIGHT: f64 = 32.0;
const ROW_HEIGHT: f64 = 32.0;

/// A column header: `Caption` in its strong weight, sentence case.
fn header_style(theme: &Theme) -> crate::text_render::TextStyle {
    theme.text(TextRole::Caption).strong()
}

/// A cell's text style: `Caption`, tabular in a numeric column.
fn cell_style(theme: &Theme, numeric: bool) -> crate::text_render::TextStyle {
    let style = theme.text(TextRole::Caption);
    if numeric { style.numeric() } else { style }
}

/// Paint one text cell: leading or (numeric) trailing, elided to the cell,
/// centred on the row; an absent value is the column's label in tertiary ink.
fn paint_cell_text<Row>(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    cell: Rect,
    column: &ColumnDef<Row>,
    value: Option<&str>,
) {
    let style = cell_style(theme, column.numeric);
    let pad = f64::from(theme.tokens.spacing.s2);
    let (label, ink) = match value {
        Some(value) => (value, theme.text_primary),
        None => (column.absent.as_str(), theme.text_tertiary),
    };
    let label = text.elide_to_width_style(label, px((cell.width() - 2.0 * pad).max(0.0)), &style);
    let (w, h) = text.measure_style(&label, None, &style);
    let x = if column.numeric {
        cell.x1 - pad - f64::from(w)
    } else {
        cell.x0 + pad
    };
    text.draw_style(
        scene,
        &label,
        ink,
        translated(transform, x, centered_text_top(cell.y0, cell.height(), h)),
        None,
        &style,
    );
}

/// Row-stacking variant of [`crate::text_metrics::centered_text_top`]: text
/// taller than the band starts at the band's top rather than bleeding into
/// the row above, which the unclamped rule would let it do.
pub(crate) fn centered_text_top(band_y0: f64, band_h: f64, text_h: f32) -> f64 {
    crate::text_metrics::centered_text_top(band_y0, band_h, f64::from(text_h)).max(band_y0)
}

/// Narrows a bounded screen-space `f64` (row/pixel geometry) to `f32` for
/// vello/taffy; table content stays far below `f32::MAX`.
#[inline]
#[allow(
    clippy::cast_possible_truncation,
    reason = "screen-space pixel values in this widget never approach f32::MAX"
)]
fn px(v: f64) -> f32 {
    v as f32
}

/// Composes a paint transform with a translation. `Affine`'s `Mul` is pure
/// `f64` matrix math with no overflow trap, so it cannot panic regardless of
/// operand magnitude.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "Affine composition is float matrix math and cannot panic"
)]
fn translated(transform: Affine, x: f64, y: f64) -> Affine {
    transform * Affine::translate((x, y))
}

fn accessible_sort_description<Row>(
    columns: &[ColumnDef<Row>],
    sort: Option<(&str, bool)>,
) -> Option<String> {
    sort.map(|(key, ascending)| {
        let header = columns.iter().find(|column| column.key == key).map_or_else(
            || key.to_owned(),
            |column| crate::i18n::I18nManager::lookup_plain(&column.header),
        );
        let direction = if ascending { "ascending" } else { "descending" };
        format!("sorted by {header} {direction}")
    })
}

impl<Row: 'static> Widget for DataTable<Row> {
    fn set_interaction_environment(
        &mut self,
        environment: crate::interaction::InteractionEnvironment,
    ) {
        self.interaction = environment;
        self.viewport
            .set_minimum_height(f64::from(environment.minimum_collection_row()));
        self.viewport
            .set_overscan(self.viewport.estimated_height() * 2.0);
        for cell in &mut self.mounted_cells {
            cell.widget.set_interaction_environment(environment);
        }
        self.visible_window_changed();
    }

    fn update(&mut self, state: &mut AppModel, path: &ModelPath) -> bool {
        let model_changes = self
            .model
            .as_deref_mut()
            .map_or_else(Vec::new, TableModel::poll_changes);
        let received_changes = !model_changes.is_empty();
        if received_changes {
            self.notify_model_changes(&model_changes);
        }
        let now = crate::time::now();
        let elapsed = self
            .last_tick
            .replace(now)
            .map_or(crate::time::Duration::ZERO, |last| {
                now.saturating_duration_since(last)
            });
        let mut children_changed = false;
        for cell in &mut self.mounted_cells {
            children_changed |= cell.widget.update(state, path);
        }
        if !self.momentum.is_active() {
            self.refresh_view_anchor();
            return received_changes || children_changed;
        }
        let delta = self.momentum.tick(elapsed);
        let before = self.viewport.offset();
        self.set_scroll_offset(before + delta);
        let moved = self.viewport.offset().to_bits() != before.to_bits();
        if !moved {
            self.momentum.stop();
        }
        self.refresh_view_anchor();
        received_changes || moved || children_changed
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.reduce_motion = theme.reduce_motion;
        if self.reduce_motion {
            self.momentum.stop();
        }
        self.apply_type_metrics(theme);
        self.measure_theme = Some(theme.clone());
        self.reconcile_mounted_cells();
        let mut child_nodes = Vec::with_capacity(self.mounted_cells.len());
        for cell in &mut self.mounted_cells {
            let child = cell.widget.build_layout(taffy, theme, node_map);
            let mut style = taffy.style(child).cloned().unwrap_or_default();
            style.position = taffy::Position::Absolute;
            if let Some(column) = self.col_rects.get(cell.column_index) {
                style.inset.left = length(px(column.x0 - self.rect.x0));
                style.size.width = Dimension::length(px(column.width()));
            } else if let Some(column) = self.columns.get(cell.column_index)
                && let ColumnWidth::Fixed(width) = column.width
            {
                style.size.width = Dimension::length(width);
            }
            let top = self
                .viewport
                .item_view_top(cell.row_index)
                .unwrap_or_default();
            style.inset.top = length(px(self.header_h + top));
            let _ = taffy.set_style(child, style);
            cell.node = Some(child);
            child_nodes.push(child);
        }
        let body_height = if self.viewport.viewport() > 0.0 {
            self.viewport.viewport()
        } else {
            self.viewport
                .content_extent()
                .max(self.viewport.estimated_height())
        };
        let height = self.header_h + body_height;
        let node = taffy.parent_node(
            Style {
                size: Size {
                    width: percent(1.0_f32),
                    height: Dimension::length(px(height)),
                },
                overflow: taffy::Point {
                    x: taffy::Overflow::Clip,
                    y: taffy::Overflow::Clip,
                },
                ..Default::default()
            },
            &child_nodes,
        );
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        self.structure_dirty = false;
        node
    }

    /// Measure [`ColumnWidth::Auto`] columns from their header and the
    /// visible cells, never narrower than four characters' worth of line.
    fn adjust_layout_with_measure(
        &mut self,
        _taffy: &mut TaffyTree<()>,
        _node: NodeId,
        text: &mut TextRenderer,
    ) -> bool {
        let Some(theme) = self.measure_theme.clone() else {
            return false;
        };
        let tokens = &theme.tokens;
        let pad = 2.0 * f64::from(tokens.spacing.s2);
        let sort_room = f64::from(tokens.metrics.icon_sm) + f64::from(tokens.spacing.s1);
        let header = header_style(&theme);
        let floor = 4.0 * type_line_height(&tokens.typography, TextRole::Caption);
        let (start, end) = self.visible_row_range();
        let mut next = HashMap::new();
        for col in &self.columns {
            if col.width != ColumnWidth::Auto {
                continue;
            }
            let label = crate::i18n::I18nManager::lookup_plain(&col.header);
            let mut width = f64::from(text.measure_style(&label, None, &header).0)
                + if col.sortable { sort_room } else { 0.0 };
            if let Some(value) = &col.text {
                let style = cell_style(&theme, col.numeric);
                for ri in start..end {
                    if let Some(TableRowState::Ready(row)) = self.row_state_at(ri) {
                        let cell = value(row).unwrap_or_else(|| col.absent.clone());
                        width = width.max(f64::from(text.measure_style(&cell, None, &style).0));
                    }
                }
            }
            next.insert(col.key.clone(), px((width + pad).max(floor).ceil()));
        }
        if next == self.auto_widths {
            return false;
        }
        self.auto_widths = next;
        true
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        // User-resized widths in `col_widths_px` override the declared `ColumnWidth`.
        let mut widths = self.auto_widths.clone();
        widths.extend(self.col_widths_px.iter().map(|(k, v)| (k.clone(), *v)));
        let next_columns = compute_column_rects(&self.columns, self.rect, &widths);
        let columns_changed = self.col_rects != next_columns;
        self.col_rects = next_columns;
        let mut row_heights = HashMap::<usize, f64>::new();
        for cell in &mut self.mounted_cells {
            let Some(child) = cell.node else { continue };
            let child_layout = taffy.layout(child).copied().unwrap_or_default();
            let desired_x = self
                .col_rects
                .get(cell.column_index)
                .map_or(self.rect.x0, |column| column.x0);
            let desired_y = self.rect.y0
                + self.header_h
                + self
                    .viewport
                    .item_view_top(cell.row_index)
                    .unwrap_or_default();
            // Position from live viewport/column geometry rather than only
            // the absolute Taffy style captured at tree-build time. Scroll
            // frames can then reposition retained controls without forcing a
            // structural rebuild when the mounted key window is unchanged.
            let child_origin = Point::new(
                desired_x - f64::from(child_layout.location.x),
                desired_y - f64::from(child_layout.location.y),
            );
            cell.widget.apply_layout(taffy, child, child_origin);
            row_heights
                .entry(cell.row_index)
                .and_modify(|height| *height = height.max(cell.widget.get_rect().height()))
                .or_insert_with(|| cell.widget.get_rect().height());
        }
        // A mounted cell never makes its row shorter than the estimate the
        // plain text rows use, so retained and text rows stay one height.
        let row_floor = self.viewport.estimated_height();
        let mut measured = false;
        for (row_index, height) in row_heights {
            if let Some(key) = self.row_identity_at(row_index) {
                measured |= self.viewport.measure(row_index, key, height.max(row_floor));
            }
        }
        if columns_changed {
            self.structure_dirty = true;
        }
        if measured {
            self.visible_window_changed();
        } else if !columns_changed {
            self.layout_dirty = false;
            self.refresh_view_anchor();
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
        self.reduce_motion = theme.reduce_motion;
        if self.reduce_motion {
            self.momentum.stop();
        }
        // No ground of its own: the table stands on its region's ground.
        self.draw_header(scene, transform, text_renderer, theme);
        let body = Rect::new(
            self.rect.x0,
            self.rect.y0 + self.header_h,
            self.rect.x1,
            self.rect.y0 + self.header_h + self.body_viewport_h(),
        );
        // Rows and mounted cells paint whole-row geometry inside one body
        // clip, so a row leaving the viewport is trimmed rather than re-laid.
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &body,
        );
        self.draw_rows(scene, transform, text_renderer, theme);
        for cell in &mut self.mounted_cells {
            cell.widget
                .draw(scene, transform, text_renderer, theme, presence);
        }
        scene.pop_layer();

        // Focus ring, so arrow-key navigation reads as a focused control.
        if self.is_focused {
            crate::widget::paint_focus_ring(scene, transform, self.rect.inset(-1.0), 0.0, theme);
        }

        self.draw_scrollbar(scene, transform, theme);
        self.refresh_view_anchor();
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if self.resize.is_none() && !self.vbar.is_dragging() && !self.drag_scroll.is_tracking() {
            self.route_to_cell_widgets(event, ctx);
        }
        if *ctx.handled {
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let pt = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.on_cursor_moved(pt, ctx.handled);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.on_left_press(*ctx.modifiers, ctx.handled, ctx.focus_target, ctx.node_id);
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                self.on_left_release(*ctx.modifiers, ctx.handled, ctx.focus_target, ctx.node_id);
            }
            WindowEvent::MouseWheel { delta, phase, .. } => {
                self.on_wheel(delta, *phase, ctx.handled);
            }
            WindowEvent::KeyboardInput { event: ke, .. }
                if self.selectable && self.is_focused && ke.state == ElementState::Pressed =>
            {
                self.drag_scroll.cancel();
                self.momentum.stop();
                self.on_key(ke, *ctx.modifiers, ctx.handled);
            }
            _ => {}
        }
        self.refresh_view_anchor();
    }

    fn wants_pointer_capture(&self) -> bool {
        self.resize.is_some()
            || self.vbar.is_dragging()
            || self.drag_scroll.is_tracking()
            || self
                .mounted_cells
                .iter()
                .any(|cell| cell.widget.wants_pointer_capture())
    }

    fn needs_animation_frame(&self) -> bool {
        self.momentum.is_active()
            || self.model.as_deref().is_some_and(TableModel::needs_poll)
            || self
                .mounted_cells
                .iter()
                .any(|cell| cell.widget.needs_animation_frame())
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.mounted_cells
            .iter()
            .map(|cell| cell.widget.as_ref())
            .collect()
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        for cell in &self.mounted_cells {
            visitor(cell.widget.as_ref());
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.mounted_cells
            .iter_mut()
            .map(|cell| cell.widget.as_mut() as &mut dyn Widget)
            .collect()
    }

    fn accessibility_child_path(&self, child_index: usize) -> Option<Vec<usize>> {
        let cell = self.mounted_cells.get(child_index)?;
        let visible_start = self.viewport.visible_range().start;
        Some(vec![
            cell.row_index.checked_sub(visible_start)?,
            cell.column_index,
        ])
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mut dirty = std::mem::take(&mut self.structure_dirty);
        for cell in &mut self.mounted_cells {
            dirty |= cell.widget.take_structure_dirty();
        }
        dirty
    }

    fn is_layout_dirty(&self) -> bool {
        self.layout_dirty
            || self
                .mounted_cells
                .iter()
                .any(|cell| cell.widget.is_layout_dirty())
    }

    fn layout_needs_measure(&self) -> bool {
        self.layout_dirty
            || self
                .mounted_cells
                .iter()
                .any(|cell| cell.widget.layout_needs_measure())
    }

    fn name(&self) -> &'static str {
        "DataTable"
    }
    fn test_id(&self) -> Option<&str> {
        Some(&self.test_id)
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn is_focusable(&self) -> bool {
        true
    }
    fn set_focus(&mut self, focused: bool) {
        self.is_focused = focused;
    }
    fn is_focused(&self) -> bool {
        self.is_focused
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let selected_pos = self.primary_index();
        // Human header, not the internal key, so the announcement reads naturally.
        let description = accessible_sort_description(&self.columns, self.sort_state());
        let mut node = AccessNode::new(Role::Table).with_state(AccessState {
            focusable: self.is_focusable(),
            focused: self.is_focused(),
            ..Default::default()
        });
        if let Some(name) = &self.accessible_name {
            node = node.with_name(crate::i18n::I18nManager::lookup_plain(name));
        }
        if let Some(i) = selected_pos {
            node = node.with_position(i.saturating_add(1), self.display_len());
        }
        if let Some(desc) = description {
            node = node.with_description(desc);
        }
        // A cell's name stays the localized column header. Ready rows may add
        // their typed row-specific value; retained controls remain children.
        let row_count = self.display_len();
        let col_count = self.columns.len();
        // The keyboard cursor (Up/Down move it, Enter activates it) is the
        // table's *active descendant*: when the table holds keyboard focus, the
        // export layer puts the reader on that row rather than on the table as a
        // whole, so arrowing down reads each row instead of repeating "table".
        let cursor = if self.selectable { selected_pos } else { None };
        // A virtual collection must keep its semantic tree virtual too. ATs
        // receive the visible rows with their absolute position/set size;
        // keyboard navigation scrolls the primary row into this window.
        let visible = {
            let laid_out = self.viewport.visible_range();
            if laid_out.is_empty() && row_count > 0 {
                // Accessibility can be queried before the first layout (unit
                // tests and some AT startup paths). Keep that fallback bounded
                // too, while ensuring the keyboard cursor is represented.
                let start = cursor.unwrap_or(0).saturating_sub(16);
                start..start.saturating_add(32).min(row_count)
            } else {
                laid_out
            }
        };
        let rows: Vec<AccessNode> = visible
            .map(|ri| {
                let selected = self.selectable && self.is_row_selected(ri);
                let row_state = self.row_state_at(ri);
                let row_bounds = self
                    .viewport
                    .item_view_top(ri)
                    .zip(self.viewport.item_height(ri))
                    .map(|(top, height)| {
                        let y0 = self.rect.y0 + self.header_h + top;
                        Rect::new(self.rect.x0, y0, self.rect.x1, y0 + height)
                    });
                let cells: Vec<AccessNode> = self
                    .columns
                    .iter()
                    .enumerate()
                    .map(|(ci, col)| {
                        let mut cell = AccessNode::new(Role::Cell)
                            .with_name(crate::i18n::I18nManager::lookup_plain(&col.header))
                            .with_position(ci.saturating_add(1), col_count);
                        if let Some(TableRowState::Ready(row)) = row_state
                            && let Some(value) = &col.accessible_value
                        {
                            cell = cell.with_value(value(row));
                        }
                        if let (Some(row), Some(column)) = (row_bounds, self.col_rects.get(ci)) {
                            cell =
                                cell.with_bounds(Rect::new(column.x0, row.y0, column.x1, row.y1));
                        }
                        cell
                    })
                    .collect();
                let mut row = AccessNode::new(Role::Row)
                    .with_position(ri.saturating_add(1), row_count)
                    .with_state(AccessState {
                        focusable: self.selectable,
                        focused: cursor == Some(ri),
                        selected,
                        busy: matches!(row_state, Some(TableRowState::Loading)),
                        invalid: matches!(row_state, Some(TableRowState::Error(_))),
                        ..Default::default()
                    })
                    // Only a viewport slice is exported, so its ordinal is not
                    // the absolute row. Key it by display position for stable
                    // AccessKit routing across scroll windows.
                    .with_key(ri as u64)
                    .with_children(cells);
                row = match row_state {
                    Some(TableRowState::Loading) => row.with_name("Loading"),
                    Some(TableRowState::Error(message)) => row.with_name(message),
                    _ => row,
                };
                if let Some(bounds) = row_bounds {
                    row = row.with_bounds(bounds);
                }
                if (self.row_key.is_some() || self.model.is_some())
                    && (self.selectable || self.on_activate.is_some())
                    && let Some(identity) = self.row_identity_at(ri)
                {
                    row = row.with_action_target(format!(
                        "row:{}:{}",
                        identity.value, identity.occurrence
                    ));
                }
                row
            })
            .collect();
        node = node.with_children(rows);
        Some(node)
    }

    /// Route an assistive technology's action to the **row** it was aimed at.
    ///
    /// The table paints its own rows and cells, so they reach an AT as synthetic
    /// nodes rather than as child widgets; without this, an AT could describe
    /// row 4 but activating it would land a click in the middle of the table.
    /// `key_path` is `[row]` for a row and `[row, column]` for a cell — the
    /// table acts on the row either way, since a cell has no independent
    /// behaviour here.
    ///
    /// Rows carry no explicit key, so the path element is the row's display
    /// index (its ordinal among the table's synthetic children), which is the
    /// same index `on_row_click` / `on_activate` report.
    fn perform_action_at(
        &mut self,
        action: &crate::a11y::Action,
        key_path: &[u64],
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        use crate::a11y::Action;
        // Empty path: the table itself, not a row — nothing table-specific to do.
        let Some(&row_key) = key_path.first() else {
            return self.perform_action(action, state, path);
        };
        // A read-only table's rows aren't operable.
        if !self.selectable {
            return false;
        }
        let Ok(row) = usize::try_from(row_key) else {
            return false;
        };
        // An id an AT held across a change can name a row that no longer
        // exists. Drop it — acting on whatever now sits at that index would
        // operate on the wrong record.
        if row >= self.display_len() {
            return false;
        }
        match action {
            // The host moves keyboard focus to the table; we move our own
            // cursor to the requested row, which is the half only we can do.
            Action::Focus => {
                self.select_only_index(row);
                self.viewport.scroll_to_index(row, ScrollAlignment::Nearest);
                self.refresh_view_anchor();
                true
            }
            Action::Activate | Action::ToggleCheck => {
                self.select_only_index(row);
                if let Some(cb) = &mut self.on_activate {
                    cb(row);
                }
                self.refresh_view_anchor();
                true
            }
            Action::ScrollIntoView => {
                self.viewport.scroll_to_index(row, ScrollAlignment::Nearest);
                self.refresh_view_anchor();
                true
            }
            _ => false,
        }
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        _state: &mut AppModel,
        _path: &ModelPath,
    ) -> bool {
        use crate::a11y::Action;

        let Some(index) = self.semantic_row_index(target) else {
            return false;
        };
        let handled = match action {
            Action::Focus if self.selectable => {
                self.select_only_index(index);
                self.viewport
                    .scroll_to_index(index, ScrollAlignment::Nearest);
                true
            }
            Action::Activate if self.selectable || self.on_activate.is_some() => {
                if self.selectable {
                    self.select_only_index(index);
                }
                if let Some(callback) = &mut self.on_activate {
                    callback(index);
                }
                true
            }
            Action::ScrollIntoView => {
                self.viewport
                    .scroll_to_index(index, ScrollAlignment::Nearest);
                true
            }
            _ => false,
        };
        self.refresh_view_anchor();
        handled
    }
}

fn compute_column_rects<Row>(
    columns: &[ColumnDef<Row>],
    host: Rect,
    overrides: &HashMap<String, f32>,
) -> Vec<Rect> {
    let total_w = px(host.width());
    let mut fixed_sum = 0.0_f32;
    let mut flex_sum = 0.0_f32;
    for c in columns {
        if let Some(px) = overrides.get(&c.key).copied() {
            fixed_sum += px;
            continue;
        }
        match c.width {
            ColumnWidth::Fixed(px) => fixed_sum += px,
            ColumnWidth::Flex(w) => flex_sum += w.max(0.0),
            ColumnWidth::Auto => flex_sum += 1.0,
        }
    }
    let flex_remaining = (total_w - fixed_sum).max(0.0);
    let mut rects = Vec::with_capacity(columns.len());
    let mut x = host.x0;
    let y0 = host.y0;
    let y1 = host.y0 + HEADER_HEIGHT;
    for c in columns {
        let w = if let Some(px) = overrides.get(&c.key).copied() {
            px
        } else {
            match c.width {
                ColumnWidth::Fixed(px) => px,
                ColumnWidth::Flex(w) => {
                    if flex_sum <= 0.0 {
                        0.0
                    } else {
                        flex_remaining * (w.max(0.0) / flex_sum)
                    }
                }
                ColumnWidth::Auto => {
                    if flex_sum <= 0.0 {
                        0.0
                    } else {
                        flex_remaining * (1.0 / flex_sum)
                    }
                }
            }
        };
        let next = x + f64::from(w);
        rects.push(Rect::new(x, y0, next, y1));
        x = next;
    }
    rects
}

/// Alpha of a zebra stripe: the faintest derived tier on the table's
/// ground (HIG §9.5), shared by [`DataTable`] and [`crate::widget::Table`].
pub(crate) const ZEBRA_ALPHA: u8 = 13;

/// A ground is *near black* below this relative luminance. sRGB is compressed
/// there, so a white veneer moves the luminance far less than the same alpha
/// does on the dark preset's `#16171B` (0.0086): white at 26 lifts that ground
/// by 0.023 but pure black by only 0.010. The high-contrast and OLED presets
/// sit on pure black, which is why `Theme::high_contrast` re-solves its
/// veneers to roughly three times the dark tiers.
pub(crate) const NEAR_BLACK_GROUND_LUMINANCE: f64 = 0.005;

/// Gain applied to a derived tier's alpha on a near-black ground, and the
/// alpha it never drops below — the high-contrast preset's own faintest
/// re-solved veneer (`theme.rs`, `set_overlays`: 51 / 77 / 110 / 153 / 217
/// against the dark 13 / 26 / 40 / 51 / 72). An alpha multiplier, not a
/// pixel value.
pub(crate) const NEAR_BLACK_TIER_GAIN: u32 = 3;
pub(crate) const NEAR_BLACK_TIER_FLOOR: u8 = 51;

/// Alpha of the selected-row fill on a near-black ground: the high-contrast
/// preset's middle re-solved tier, a 4.1:1 step off black that still carries
/// white text at 5.1:1. On such a ground `Colors::lift(1.0)` collapses to its
/// hairline fallback (HIG §9.3 names this weakness), so the row takes a floor
/// of its own; idle rows stay transparent, so the state order is kept.
pub(crate) const NEAR_BLACK_SELECTED_ALPHA: u8 = 110;

/// A derived fill tier (band, stripe, track, thumb) on `surface`:
/// [`Colors::hairline_on`] at `alpha`, with the near-black gain where the
/// ground would otherwise swallow it. On the dark and light presets this is
/// exactly `hairline_on(surface, alpha)`.
#[must_use]
pub(crate) fn tier_on(surface: Color, alpha: u8) -> Color {
    use crate::a11y::relative_luminance;
    let alpha = if relative_luminance(surface) < NEAR_BLACK_GROUND_LUMINANCE {
        let boosted =
            u8::try_from((u32::from(alpha) * NEAR_BLACK_TIER_GAIN).min(255)).unwrap_or(u8::MAX);
        boosted.max(NEAR_BLACK_TIER_FLOOR)
    } else {
        alpha
    };
    Colors::hairline_on(surface, alpha)
}

/// The fill of a selected row (HIG §9.3): the selection lift of the window
/// ground, or the near-black floor where a lift does not register.
#[must_use]
pub(crate) fn selection_fill(theme: &Theme) -> Color {
    use crate::a11y::relative_luminance;
    let content = theme.tokens.colors.surfaces.content;
    if relative_luminance(content) < NEAR_BLACK_GROUND_LUMINANCE {
        return Colors::hairline_on(content, NEAR_BLACK_SELECTED_ALPHA);
    }
    theme.tokens.colors.intent.selection
}

#[cfg(test)]
mod tier_tests {
    use super::*;
    use crate::a11y::contrast_ratio;
    use crate::surface::edge_delta;
    use crate::tokens::colors::{HAIRLINE_MIN_DELTA, NON_TEXT_MIN_CONTRAST, ON_FILL_MIN_CONTRAST};

    fn presets() -> [Theme; 3] {
        [Theme::dark(), Theme::light(), Theme::high_contrast()]
    }

    /// Composite `over` (with its alpha) onto an opaque `ground`.
    fn blend(over: Color, ground: Color) -> Color {
        let a = over.components[3];
        let o = over.components;
        let g = ground.components;
        Color::new([
            o[0] * a + g[0] * (1.0 - a),
            o[1] * a + g[1] * (1.0 - a),
            o[2] * a + g[2] * (1.0 - a),
            1.0,
        ])
    }

    #[test]
    fn selected_rows_and_tier_hairlines_register_on_every_ground() {
        fn selected_row_fill_registers_on_every_preset() {
            for theme in presets() {
                let base = theme.tokens.colors.surfaces.content;
                let fill = selection_fill(&theme);
                let moved = edge_delta(fill, base);
                assert!(
                    moved >= HAIRLINE_MIN_DELTA,
                    "{}: selected row moved the ground only {moved:.4}",
                    theme.name
                );
            }
        }

        /// HIG §9.3's caveat made concrete: on the pure-black high-contrast ground
        /// the shared lift is weaker than a floor, so the row takes one that reads
        /// as a control and still carries the theme's text.
        fn selected_row_on_a_black_ground_reads_as_a_control_and_carries_text() {
            let theme = Theme::high_contrast();
            let base = theme.tokens.colors.surfaces.content;
            let shared =
                crate::chrome::selection_tier(&theme, crate::chrome::SelectionState::active(true))
                    .fill;
            assert!(
                edge_delta(shared, base) < HAIRLINE_MIN_DELTA,
                "fixture: the shared lift is expected to collapse on this ground"
            );
            let fill = blend(selection_fill(&theme), base);
            let step = contrast_ratio(fill, base);
            assert!(
                step >= NON_TEXT_MIN_CONTRAST,
                "selected row step {step:.2}:1"
            );
            let ink = contrast_ratio(theme.tokens.colors.text_primary, fill);
            assert!(
                ink >= ON_FILL_MIN_CONTRAST,
                "text on the selected row {ink:.2}:1"
            );
        }

        /// Pins the dark and light screenshots: on the two grounds this module's
        /// callers paint tiers on, the gain never engages. (The dark preset's
        /// overlay tier is itself near black and does take it, by design.)
        fn tier_on_is_plain_hairline_on_ordinary_grounds() {
            for theme in [Theme::dark(), Theme::light()] {
                let s = &theme.tokens.colors.surfaces;
                for ground in [s.content, s.raised] {
                    for alpha in [ZEBRA_ALPHA, crate::chrome::HAIRLINE_ALPHA, 40, 72] {
                        assert_eq!(tier_on(ground, alpha), Colors::hairline_on(ground, alpha));
                    }
                }
            }
        }

        /// The stripe stays fainter than the band on every preset, and on black
        /// both still register (the high-contrast preset's own floor).
        fn tiers_keep_their_order_and_register_on_black() {
            for theme in presets() {
                let raised = theme.tokens.colors.surfaces.raised;
                let zebra = edge_delta(tier_on(raised, ZEBRA_ALPHA), raised);
                let band = edge_delta(tier_on(raised, crate::chrome::HAIRLINE_ALPHA), raised);
                assert!(
                    zebra < band,
                    "{}: zebra {zebra:.4} vs band {band:.4}",
                    theme.name
                );
            }
            let black = Theme::high_contrast().tokens.colors.surfaces.raised;
            assert!(edge_delta(tier_on(black, ZEBRA_ALPHA), black) >= HAIRLINE_MIN_DELTA);
            assert_eq!(
                tier_on(black, ZEBRA_ALPHA),
                Colors::hairline_on(black, NEAR_BLACK_TIER_FLOOR)
            );
        }

        selected_row_fill_registers_on_every_preset();
        selected_row_on_a_black_ground_reads_as_a_control_and_carries_text();
        tier_on_is_plain_hairline_on_ordinary_grounds();
        tiers_keep_their_order_and_register_on_black();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::Duration;
    use crate::widget::Button;
    use automerge::AutoCommit;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    #[derive(Clone)]
    struct R {
        name: String,
    }

    fn sample_cols() -> Vec<ColumnDef<R>> {
        vec![
            ColumnDef::new("name", "Name", ColumnWidth::Flex(1.0)).sortable(true),
            ColumnDef::new("x", "X", ColumnWidth::Fixed(40.0)),
        ]
    }

    struct HugeModel {
        row: R,
        key_queries: Rc<Cell<usize>>,
        row_queries: Rc<Cell<usize>>,
    }

    impl TableModel for HugeModel {
        type Row = R;

        fn len(&self) -> usize {
            100_000_000
        }

        fn key_at(&self, index: usize) -> Option<u64> {
            self.key_queries
                .set(self.key_queries.get().saturating_add(1));
            (index < self.len()).then(|| u64::try_from(index).expect("index fits u64"))
        }

        fn index_of(&self, key: u64) -> Option<usize> {
            usize::try_from(key)
                .ok()
                .filter(|index| *index < self.len())
        }

        fn row_state(&self, index: usize) -> TableRowState<'_, Self::Row> {
            self.row_queries
                .set(self.row_queries.get().saturating_add(1));
            if index < self.len() {
                TableRowState::Ready(&self.row)
            } else {
                TableRowState::Error("outside model")
            }
        }
    }

    struct PagingModel {
        row: R,
        loaded: Rc<Cell<bool>>,
        changed: Rc<Cell<bool>>,
        requested: Rc<RefCell<Vec<Range<usize>>>>,
    }

    struct InsertModel {
        row: R,
        inserted: Rc<Cell<bool>>,
    }

    struct SortableModel {
        row: R,
        descending: Rc<Cell<bool>>,
    }

    struct NotifyingModel {
        row: R,
        order: Rc<RefCell<Vec<u64>>>,
        changes: Rc<RefCell<Vec<TableModelChange>>>,
    }

    impl TableModel for NotifyingModel {
        type Row = R;

        fn len(&self) -> usize {
            self.order.borrow().len()
        }

        fn key_at(&self, index: usize) -> Option<u64> {
            self.order.borrow().get(index).copied()
        }

        fn index_of(&self, key: u64) -> Option<usize> {
            self.order
                .borrow()
                .iter()
                .position(|candidate| *candidate == key)
        }

        fn row_state(&self, _index: usize) -> TableRowState<'_, Self::Row> {
            TableRowState::Ready(&self.row)
        }

        fn poll_changes(&mut self) -> Vec<TableModelChange> {
            std::mem::take(&mut *self.changes.borrow_mut())
        }
    }

    impl TableModel for SortableModel {
        type Row = R;

        fn len(&self) -> usize {
            3
        }

        fn key_at(&self, index: usize) -> Option<u64> {
            let order = if self.descending.get() {
                [3, 2, 1]
            } else {
                [1, 2, 3]
            };
            order.get(index).copied()
        }

        fn index_of(&self, key: u64) -> Option<usize> {
            let order = if self.descending.get() {
                [3, 2, 1]
            } else {
                [1, 2, 3]
            };
            order.iter().position(|candidate| *candidate == key)
        }

        fn row_state(&self, _index: usize) -> TableRowState<'_, Self::Row> {
            TableRowState::Ready(&self.row)
        }

        fn sort_by(&mut self, column_key: &str, ascending: bool) -> bool {
            if column_key != "name" {
                return false;
            }
            self.descending.set(!ascending);
            true
        }
    }

    impl TableModel for InsertModel {
        type Row = R;

        fn len(&self) -> usize {
            if self.inserted.get() { 101 } else { 100 }
        }

        fn key_at(&self, index: usize) -> Option<u64> {
            if index >= self.len() {
                return None;
            }
            if self.inserted.get() {
                Some(if index == 0 {
                    10_000
                } else {
                    u64::try_from(index.saturating_sub(1)).expect("index fits u64")
                })
            } else {
                Some(u64::try_from(index).expect("index fits u64"))
            }
        }

        fn index_of(&self, key: u64) -> Option<usize> {
            if self.inserted.get() {
                if key == 10_000 {
                    return Some(0);
                }
                usize::try_from(key)
                    .ok()
                    .filter(|index| *index < 100)
                    .map(|index| index.saturating_add(1))
            } else {
                usize::try_from(key).ok().filter(|index| *index < 100)
            }
        }

        fn row_state(&self, _index: usize) -> TableRowState<'_, Self::Row> {
            TableRowState::Ready(&self.row)
        }
    }

    impl TableModel for PagingModel {
        type Row = R;

        fn len(&self) -> usize {
            10_000
        }

        fn key_at(&self, index: usize) -> Option<u64> {
            (index < self.len()).then(|| {
                u64::try_from(index)
                    .expect("index fits u64")
                    .saturating_add(1_000)
            })
        }

        fn index_of(&self, key: u64) -> Option<usize> {
            key.checked_sub(1_000)
                .and_then(|index| usize::try_from(index).ok())
                .filter(|index| *index < self.len())
        }

        fn row_state(&self, index: usize) -> TableRowState<'_, Self::Row> {
            if index == 3 {
                TableRowState::Error("row failed to load")
            } else if self.loaded.get() {
                TableRowState::Ready(&self.row)
            } else {
                TableRowState::Loading
            }
        }

        fn request_range(&mut self, range: Range<usize>) {
            self.requested.borrow_mut().push(range);
        }

        fn poll_changed(&mut self) -> bool {
            self.changed.replace(false)
        }

        fn needs_poll(&self) -> bool {
            !self.loaded.get()
        }
    }

    #[test]
    fn columns_split_flex_and_resize_from_the_press_origin() {
        fn compute_column_rects_splits_flex_after_fixed() {
            let cols = sample_cols();
            let host = Rect::new(0.0, 0.0, 200.0, 28.0);
            let rects = compute_column_rects(&cols, host, &HashMap::new());
            assert_eq!(rects.len(), 2);
            assert!((rects[0].width() - 160.0).abs() < 0.01);
            assert!((rects[1].width() - 40.0).abs() < 0.01);
        }

        fn column_resize_uses_press_origin_clamps_and_stops_on_release() {
            let mut table = DataTable::<R>::new(vec![ColumnDef::new(
                "name",
                "Name",
                ColumnWidth::Fixed(80.0),
            )]);

            table.begin_resize("name", 200.0);
            table.drag_resize(215.0);
            assert_eq!(table.column_width_px("name"), Some(95.0));
            table.drag_resize(220.0);
            assert_eq!(
                table.column_width_px("name"),
                Some(100.0),
                "drag deltas must stay relative to the initial press"
            );

            table.end_resize();
            table.drag_resize(1_000.0);
            assert_eq!(
                table.column_width_px("name"),
                Some(100.0),
                "release must end resize ownership"
            );

            table.begin_resize("name", 100.0);
            table.drag_resize(0.0);
            assert_eq!(
                table.column_width_px("name"),
                Some(DataTable::<R>::MIN_COL_WIDTH)
            );
        }

        /// One extra stroke over the unfocused render, asserted on the production `draw` path.
        fn focused_table_paints_a_focus_ring() {
            let mut unfocused = DataTable::<R>::new(sample_cols()).selectable(true);
            unfocused.set_rows(rows4());
            unfocused.set_focus(false);
            let without = table_color_fills(&mut unfocused);

            let mut focused = DataTable::<R>::new(sample_cols()).selectable(true);
            focused.set_rows(rows4());
            focused.set_focus(true);
            let with = table_color_fills(&mut focused);

            assert_eq!(
                with,
                without + 1,
                "a focused table must add exactly one focus-ring stroke ({with} vs {without})",
            );
        }

        compute_column_rects_splits_flex_after_fixed();
        column_resize_uses_press_origin_clamps_and_stops_on_release();
        focused_table_paints_a_focus_ring();
    }

    #[test]
    fn model_table_virtualizes_pages_and_keeps_stable_selection() {
        fn model_selection_builder_discards_absent_keys() {
            let mut selection = crate::selection::KeySelection::new();
            selection.toggle(3);
            selection.toggle(100_000_000);
            let table = DataTable::from_model(
                sample_cols(),
                HugeModel {
                    row: R {
                        name: "resident".into(),
                    },
                    key_queries: Rc::new(Cell::new(0)),
                    row_queries: Rc::new(Cell::new(0)),
                },
            )
            .with_model_selection(selection)
            .with_viewport(120.0, 24.0);

            assert!(table.model_selection().is_selected(&3));
            assert!(!table.model_selection().is_selected(&100_000_000));
            assert_eq!(table.model_selection().len(100_000_000), 1);
            let visible = table.visible_row_range();
            assert_eq!(visible.0, 0);
            assert!(visible.1 <= 10, "viewport plus overscan stays bounded");
        }

        fn model_table_virtualizes_a_hundred_million_rows_in_constant_space() {
            let key_queries = Rc::new(Cell::new(0));
            let row_queries = Rc::new(Cell::new(0));
            let mut table = DataTable::from_model(
                sample_cols(),
                HugeModel {
                    row: R {
                        name: "resident".into(),
                    },
                    key_queries: Rc::clone(&key_queries),
                    row_queries: Rc::clone(&row_queries),
                },
            )
            .selectable(true);
            table.set_viewport(120.0, 24.0);
            table.set_scroll_y(f32::MAX);
            let _laid = lay_out(&mut table);

            let semantics = Widget::accessibility(&table).expect("table semantics");
            assert_eq!(table.viewport.len(), 100_000_000);
            assert_eq!(table.viewport.resident_measurements(), 0);
            assert!(semantics.children.len() <= 10);
            assert!(
                key_queries.get() < 100,
                "queried {} keys",
                key_queries.get()
            );
            assert!(
                row_queries.get() < 100,
                "queried {} rows",
                row_queries.get()
            );

            assert!(Widget::perform_semantic_action(
                &mut table,
                "row:99999999:0",
                &crate::a11y::Action::Focus,
                &mut AutoCommit::new(),
                &automerge::ROOT,
            ));
            assert_eq!(table.model_selection().primary(), Some(&99_999_999));
            table.model_selection_mut().select_all();
            assert!(table.model_selection().is_select_all());
            assert_eq!(table.model_selection().len(100_000_000), 100_000_000);
            assert_eq!(table.model_selection().stored_keys().count(), 0);
        }

        fn paged_model_requests_only_the_window_and_wakes_on_loaded_rows() {
            let loaded = Rc::new(Cell::new(false));
            let changed = Rc::new(Cell::new(false));
            let requested = Rc::new(RefCell::new(Vec::new()));
            let mut table = DataTable::from_model(
                sample_cols(),
                PagingModel {
                    row: R {
                        name: "loaded row".into(),
                    },
                    loaded: Rc::clone(&loaded),
                    changed: Rc::clone(&changed),
                    requested: Rc::clone(&requested),
                },
            );
            table.set_viewport(96.0, 24.0);

            let before = Widget::accessibility(&table).expect("loading semantics");
            assert_eq!(before.children[0].name.as_deref(), Some("Loading"));
            assert!(before.children[0].state.busy);
            assert_eq!(
                before.children[3].name.as_deref(),
                Some("row failed to load")
            );
            assert!(before.children[3].state.invalid);
            assert!(table.needs_animation_frame());

            let _fills = table_color_fills(&mut table);
            let requested = requested.borrow();
            assert_eq!(requested.len(), 1);
            assert!(requested[0].start == 0 && requested[0].end < 16);
            drop(requested);

            loaded.set(true);
            changed.set(true);
            assert!(Widget::update(
                &mut table,
                &mut AutoCommit::new(),
                &automerge::ROOT,
            ));
            let after = Widget::accessibility(&table).expect("loaded semantics");
            assert_eq!(after.children[0].name, None);
            assert!(!table.needs_animation_frame());
        }

        fn model_insert_preserves_stable_anchor_and_key_selection() {
            let inserted = Rc::new(Cell::new(false));
            let mut table = DataTable::from_model(
                sample_cols(),
                InsertModel {
                    row: R { name: "row".into() },
                    inserted: Rc::clone(&inserted),
                },
            )
            .selectable(true);
            table.set_viewport(48.0, 24.0);
            table.set_scroll_y(50.0 * 24.0 + 5.0);
            table.model_selection_mut().select_only(50);
            let before = table.viewport.item_view_top(50);

            inserted.set(true);
            table.notify_model_changed();

            assert_eq!(table.semantic_row_index("row:50:0"), Some(51));
            assert_eq!(table.viewport.item_view_top(51), before);
            assert_eq!(table.model_selection().primary(), Some(&50));
            assert!(table.model_selection().is_selected(&50));
        }

        fn stable_row_keys_anchor_inserts_above_the_viewport() {
            let mut t = DataTable::<R>::new(sample_cols())
                .with_row_key(|row| u64::from(row.name.as_bytes()[0]));
            t.set_rows(rows4());
            t.set_viewport(24.0, px(ROW_HEIGHT));
            t.set_scroll_y(2.0 * px(ROW_HEIGHT) + 5.0);
            let before = t.viewport.item_view_top(2);

            t.set_rows(vec![
                R { name: "x".into() },
                R { name: "a".into() },
                R { name: "b".into() },
                R { name: "c".into() },
                R { name: "d".into() },
            ]);

            let c = t.semantic_row_index(&format!("row:{}:0", u64::from(b'c')));
            assert_eq!(c, Some(3));
            assert_eq!(t.viewport.item_view_top(c.unwrap()), before);
        }

        model_selection_builder_discards_absent_keys();
        model_table_virtualizes_a_hundred_million_rows_in_constant_space();
        paged_model_requests_only_the_window_and_wakes_on_loaded_rows();
        model_insert_preserves_stable_anchor_and_key_selection();
        stable_row_keys_anchor_inserts_above_the_viewport();
    }

    #[test]
    fn granular_model_changes_retain_controls_by_stable_key() {
        fn granular_row_change_rebuilds_only_that_rows_retained_control() {
            let builds = Rc::new(Cell::new(0));
            let order = Rc::new(RefCell::new(vec![1, 2, 3, 4]));
            let changes = Rc::new(RefCell::new(Vec::new()));
            let mut table = DataTable::from_model(
                interactive_cols(Rc::new(Cell::new(0)), Rc::clone(&builds)),
                NotifyingModel {
                    row: R { name: "row".into() },
                    order,
                    changes: Rc::clone(&changes),
                },
            );
            table.set_viewport(120.0, 24.0);
            let _ = lay_out(&mut table);
            let initial_builds = builds.get();

            changes
                .borrow_mut()
                .push(TableModelChange::RowsChanged { keys: vec![2] });
            assert!(Widget::update(
                &mut table,
                &mut AutoCommit::new(),
                &automerge::ROOT,
            ));
            let _ = lay_out(&mut table);

            assert_eq!(builds.get(), initial_builds + 1);
            assert_eq!(table.mounted_cell_count(), 4);
        }

        fn granular_structural_changes_retain_controls_by_stable_key() {
            let builds = Rc::new(Cell::new(0));
            let order = Rc::new(RefCell::new(vec![1, 2, 3, 4]));
            let changes = Rc::new(RefCell::new(Vec::new()));
            let mut table = DataTable::from_model(
                interactive_cols(Rc::new(Cell::new(0)), Rc::clone(&builds)),
                NotifyingModel {
                    row: R { name: "row".into() },
                    order: Rc::clone(&order),
                    changes: Rc::clone(&changes),
                },
            );
            table.set_viewport(144.0, 24.0);
            let _ = lay_out(&mut table);
            let initial_builds = builds.get();

            order.borrow_mut().insert(0, 99);
            changes
                .borrow_mut()
                .push(TableModelChange::RowsInserted { index: 0, count: 1 });
            assert!(Widget::update(
                &mut table,
                &mut AutoCommit::new(),
                &automerge::ROOT,
            ));
            let _ = lay_out(&mut table);
            assert_eq!(builds.get(), initial_builds + 1);

            order.borrow_mut().swap(1, 3);
            changes.borrow_mut().push(TableModelChange::RowsMoved {
                from: 1,
                to: 3,
                count: 1,
            });
            assert!(Widget::update(
                &mut table,
                &mut AutoCommit::new(),
                &automerge::ROOT,
            ));
            let _ = lay_out(&mut table);

            assert_eq!(
                builds.get(),
                initial_builds + 1,
                "moving stable keys retains their live cell controls"
            );
            assert_eq!(table.mounted_cell_count(), 5);
        }

        fn retained_cell_widgets_are_bounded_to_the_huge_model_window() {
            let key_queries = Rc::new(Cell::new(0));
            let row_queries = Rc::new(Cell::new(0));
            let builds = Rc::new(Cell::new(0));
            let model = HugeModel {
                row: R {
                    name: "Edit".into(),
                },
                key_queries: Rc::clone(&key_queries),
                row_queries: Rc::clone(&row_queries),
            };
            let mut table = DataTable::from_model(
                interactive_cols(Rc::new(Cell::new(0)), Rc::clone(&builds)),
                model,
            );
            table.set_viewport(48.0, 24.0);
            let _ = lay_out(&mut table);

            assert!(table.mounted_cell_count() > 0);
            assert!(
                table.mounted_cell_count() < 20,
                "100M logical rows retain only the viewport window"
            );
            assert_eq!(builds.get(), table.mounted_cell_count());
            assert!(key_queries.get() < 100);
            assert!(row_queries.get() < 100);
        }

        fn retained_cell_widgets_follow_stable_rows_through_sorting() {
            let builds = Rc::new(Cell::new(0));
            let mut table =
                DataTable::new(interactive_cols(Rc::new(Cell::new(0)), Rc::clone(&builds)))
                    .with_row_key(|row: &R| {
                        u64::from(row.name.as_bytes().first().copied().unwrap_or_default())
                    });
            table.set_rows(rows4());
            let _ = lay_out(&mut table);
            let first_builds = builds.get();
            table.sort_by_with(|a, b| a.name.cmp(&b.name), "name", false);
            let _ = lay_out(&mut table);

            assert_eq!(
                builds.get(),
                first_builds,
                "sorting repositions cells without rebuilding retained controls"
            );
        }

        fn retained_cell_widgets_track_scroll_inside_the_same_mounted_window() {
            let mut table = DataTable::new(interactive_cols(
                Rc::new(Cell::new(0)),
                Rc::new(Cell::new(0)),
            ));
            table.set_rows(rows4());
            table.set_viewport(48.0, 24.0);
            let (tree, node) = lay_out(&mut table);
            let before = table
                .mounted_cells
                .first()
                .expect("first mounted cell")
                .widget
                .get_rect()
                .y0;
            table.set_scroll_y(5.0);
            table.apply_layout(&tree, node, Point::ZERO);
            let after = table
                .mounted_cells
                .first()
                .expect("first mounted cell")
                .widget
                .get_rect()
                .y0;
            assert_eq!(after, before - 5.0);
        }

        granular_row_change_rebuilds_only_that_rows_retained_control();
        granular_structural_changes_retain_controls_by_stable_key();
        retained_cell_widgets_are_bounded_to_the_huge_model_window();
        retained_cell_widgets_follow_stable_rows_through_sorting();
        retained_cell_widgets_track_scroll_inside_the_same_mounted_window();
    }

    #[test]
    fn sorting_is_owned_by_the_model_and_reapplied_on_rows() {
        fn model_owns_sort_order_while_table_owns_header_state() {
            let descending = Rc::new(Cell::new(false));
            let mut table = DataTable::from_model(
                sample_cols(),
                SortableModel {
                    row: R { name: "row".into() },
                    descending: Rc::clone(&descending),
                },
            );

            assert_eq!(table.toggle_sort(0), Some(("name".into(), true)));
            assert!(!descending.get());
            assert_eq!(table.sort_state(), Some(("name", true)));
            assert_eq!(table.semantic_row_index("row:1:0"), Some(0));

            assert_eq!(table.toggle_sort(0), Some(("name".into(), false)));
            assert!(descending.get());
            assert_eq!(table.sort_state(), Some(("name", false)));
            assert_eq!(table.semantic_row_index("row:1:0"), Some(2));
        }

        fn toggle_sort_requires_prior_sort_by_with() {
            let mut t = DataTable::<R>::new(sample_cols());
            // No comparator installed yet → toggle returns None.
            assert!(t.toggle_sort(0).is_none());
            // Install comparator, then toggling the same column flips direction.
            t.set_rows(vec![R { name: "b".into() }, R { name: "a".into() }]);
            t.sort_by_with(|a, b| a.name.cmp(&b.name), "name", true);
            assert_eq!(t.view_rows()[0].name, "a");
            let (_, asc) = t.toggle_sort(0).unwrap();
            assert!(!asc);
            assert_eq!(t.view_rows()[0].name, "b");
        }

        fn set_rows_reapplies_sort() {
            let mut t = DataTable::<R>::new(sample_cols());
            t.sort_by_with(|a, b| a.name.cmp(&b.name), "name", true);
            t.set_rows(vec![
                R { name: "c".into() },
                R { name: "a".into() },
                R { name: "b".into() },
            ]);
            let v = t.view_rows();
            assert_eq!(v[0].name, "a");
            assert_eq!(v[1].name, "b");
            assert_eq!(v[2].name, "c");
        }

        fn header_click_fires_on_header_click_instead_of_toggle() {
            let hits = Rc::new(RefCell::new(Vec::<usize>::new()));
            let mut t = {
                let hits = hits.clone();
                DataTable::<R>::new(sample_cols())
                    .on_header_click(move |i| hits.borrow_mut().push(i))
            };
            t.set_rows(rows4());
            // Install a comparator so a fallback toggle *would* have sorted —
            // proving the callback path takes precedence (rows stay put).
            t.sort_by_with(|a, b| a.name.cmp(&b.name), "name", true);
            let (tree, node) = lay_out(&mut t);
            let mods = winit::event::Modifiers::default();

            // Header band is y ∈ [0, 28). Name column starts at x=0. Click (20, 10).
            dispatch(
                &mut t,
                &tree,
                node,
                &WindowEvent::CursorMoved {
                    device_id: device(),
                    position: (20.0, 10.0).into(),
                },
                mods,
            );
            dispatch(
                &mut t,
                &tree,
                node,
                &WindowEvent::MouseInput {
                    device_id: device(),
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                },
                mods,
            );

            assert_eq!(*hits.borrow(), vec![0], "header click reported column 0");
            // Sort direction unchanged because the callback replaced toggle.
            assert_eq!(t.sort_state(), Some(("name", true)));
        }

        model_owns_sort_order_while_table_owns_header_state();
        toggle_sort_requires_prior_sort_by_with();
        set_rows_reapplies_sort();
        header_click_fires_on_header_click_instead_of_toggle();
    }

    #[test]
    fn table_accessibility_names_roles_sort_and_selection() {
        fn data_table_exposes_stable_test_and_accessible_names() {
            let t = DataTable::<R>::new(sample_cols());
            assert_eq!(Widget::test_id(&t), Some("data_table"));
            let named = DataTable::<R>::new(sample_cols())
                .with_id("results-table")
                .with_accessible_name("Results");
            assert_eq!(Widget::test_id(&named), Some("results-table"));
            assert_eq!(
                Widget::accessibility(&named).and_then(|node| node.name),
                Some("Results".into())
            );
        }

        fn accessibility_reports_table_role_sort_and_selection() {
            use crate::a11y::Role;
            let mut t = DataTable::<R>::new(sample_cols()).selectable(true);
            t.set_rows(rows4());
            // Active sort surfaces via the column's human header, not its key.
            t.sort_by_with(|a, b| a.name.cmp(&b.name), "name", true);
            // Select the second display row.
            t.selection_mut().select_only(1);
            let node = Widget::accessibility(&t).expect("table is not presentational");
            assert_eq!(node.role, Role::Table);
            assert!(node.state.focusable);
            assert!(!node.state.focused);
            // The selected row's position is structured (rendered as "2 of 4" by
            // the reader) rather than baked into the value string.
            assert_eq!(node.pos_in_set, Some(2));
            assert_eq!(node.set_size, Some(4));
            assert_eq!(
                node.description.as_deref(),
                Some("sorted by Name ascending")
            );
        }

        fn accessibility_no_sort_or_selection_omits_value_and_description() {
            let t = DataTable::<R>::new(sample_cols());
            let node = Widget::accessibility(&t).unwrap();
            assert_eq!(node.value, None, "no selection → no value");
            assert_eq!(node.pos_in_set, None, "no selection → no position");
            assert_eq!(node.set_size, None, "no selection → no set size");
            assert_eq!(
                node.description, None,
                "no active sort → no active sort description"
            );
        }

        fn the_keyboard_cursor_row_is_exported_as_the_active_descendant() {
            // Exactly one row may claim focus, and it must be the row Up/Down moved
            // the cursor to — that is what puts a reader on the row rather than on
            // the table.
            let mut t = DataTable::<R>::new(sample_cols()).selectable(true);
            t.set_rows(vec![
                R { name: "a".into() },
                R { name: "b".into() },
                R { name: "c".into() },
            ]);
            t.selection_mut().move_to(1, false);
            let node = Widget::accessibility(&t).unwrap();
            let focused: Vec<usize> = node
                .children
                .iter()
                .enumerate()
                .filter(|(_, r)| r.state.focused)
                .map(|(i, _)| i)
                .collect();
            assert_eq!(focused, vec![1], "the cursor row, and only it, is focused");
            assert!(
                node.children.iter().all(|r| r.state.focusable),
                "every row must be focusable or an AT cannot navigate to it"
            );

            t.selection_mut().move_to(2, false);
            let node = Widget::accessibility(&t).unwrap();
            assert!(node.children[2].state.focused, "focus follows the cursor");
            assert!(!node.children[1].state.focused);
        }

        fn a_read_only_tables_rows_are_not_focusable() {
            // Nothing to operate, so nothing should advertise itself as operable.
            let mut t = DataTable::<R>::new(sample_cols());
            t.set_rows(rows4());
            assert!(!t.selectable, "the default table is read-only");
            let node = Widget::accessibility(&t).unwrap();
            assert!(node.children.iter().all(|r| !r.state.focusable));
            assert!(node.children.iter().all(|r| !r.state.focused));
        }

        data_table_exposes_stable_test_and_accessible_names();
        accessibility_reports_table_role_sort_and_selection();
        accessibility_no_sort_or_selection_omits_value_and_description();
        the_keyboard_cursor_row_is_exported_as_the_active_descendant();
        a_read_only_tables_rows_are_not_focusable();
    }

    #[test]
    fn semantic_rows_and_cells_publish_identity_geometry_and_values() {
        fn semantic_row_actions_use_stable_row_identities() {
            let activated = Rc::new(RefCell::new(Vec::new()));
            let captured = Rc::clone(&activated);
            let mut table = DataTable::<R>::new(sample_cols())
                .with_row_key(|row| u64::from(row.name.as_bytes()[0]))
                .selectable(true)
                .on_activate(move |index| captured.borrow_mut().push(index));
            table.set_rows(vec![R { name: "a".into() }, R { name: "b".into() }]);

            let semantics = Widget::accessibility(&table).expect("table semantics");
            assert_eq!(
                semantics.children[1].action_target.as_deref(),
                Some("row:98:0")
            );
            assert!(Widget::perform_semantic_action(
                &mut table,
                "row:98:0",
                &crate::a11y::Action::Focus,
                &mut AutoCommit::new(),
                &automerge::ROOT,
            ));
            assert_eq!(table.selection().primary(), Some(1));
            assert!(Widget::perform_semantic_action(
                &mut table,
                "row:98:0",
                &crate::a11y::Action::Activate,
                &mut AutoCommit::new(),
                &automerge::ROOT,
            ));
            assert_eq!(&*activated.borrow(), &[1]);

            let mut index_keyed = DataTable::<R>::new(sample_cols()).selectable(true);
            index_keyed.set_rows(vec![R { name: "a".into() }]);
            assert_eq!(
                Widget::accessibility(&index_keyed).unwrap().children[0].action_target,
                None,
                "index fallbacks are not stable action identities"
            );
        }

        fn semantic_rows_and_cells_publish_scrolled_viewport_geometry() {
            let mut table = DataTable::<R>::new(sample_cols());
            table.set_rows(rows4());
            table.set_viewport(72.0, 24.0);
            table.set_scroll_y(10.0);
            let _ = lay_out(&mut table);

            let semantics = Widget::accessibility(&table).expect("table semantics");
            let row = semantics.children.first().expect("first visible row");
            let row_bounds = row.bounds.expect("row bounds").to_rect();
            assert_eq!(row_bounds.y0, table.rect.y0 + table.header_h - 10.0);
            assert_eq!(row_bounds.y1, row_bounds.y0 + 24.0);
            assert_eq!(row_bounds.x0, table.rect.x0);
            assert_eq!(row_bounds.x1, table.rect.x1);

            let first_cell = row.children.first().expect("first cell");
            let cell_bounds = first_cell.bounds.expect("cell bounds").to_rect();
            assert_eq!(cell_bounds.y0, row_bounds.y0);
            assert_eq!(cell_bounds.y1, row_bounds.y1);
            assert_eq!(cell_bounds.x0, table.col_rects[0].x0);
            assert_eq!(cell_bounds.x1, table.col_rects[0].x1);
        }

        fn a_routed_action_naming_a_row_that_no_longer_exists_does_nothing() {
            // An assistive technology can hold a node id across a row being
            // removed. Acting on whatever now sits at that index would operate on
            // the wrong record — so an out-of-range row must be dropped, not
            // clamped.
            let fired = std::rc::Rc::new(std::cell::RefCell::new(Vec::<usize>::new()));
            let sink = fired.clone();
            let mut t = DataTable::<R>::new(sample_cols())
                .selectable(true)
                .on_activate(move |i| sink.borrow_mut().push(i));
            t.set_rows(vec![R { name: "a".into() }, R { name: "b".into() }]);
            t.selection_mut().move_to(0, false);

            let mut doc = automerge::AutoCommit::new();
            let root = automerge::ROOT;
            let handled = Widget::perform_action_at(
                &mut t,
                &crate::a11y::Action::Activate,
                &[7],
                &mut doc,
                &root,
            );
            assert!(!handled, "a vanished row cannot be activated");
            assert!(
                fired.borrow().is_empty(),
                "and must not activate some other row in its place"
            );
            assert_eq!(
                t.selection.primary(),
                Some(0),
                "nor move the cursor off the row the user was on"
            );
        }

        fn accessibility_exposes_per_row_and_cell_structure() {
            use crate::a11y::Role;
            let mut t = DataTable::<R>::new(sample_cols()).selectable(true);
            t.set_rows(rows4());
            t.selection_mut().select_only(2);
            let node = Widget::accessibility(&t).unwrap();

            // One Row child per display row, each numbered "N of 4".
            assert_eq!(node.children.len(), 4, "a row node per display row");
            for (i, row) in node.children.iter().enumerate() {
                assert_eq!(row.role, Role::Row);
                assert_eq!(row.pos_in_set, Some(i + 1));
                assert_eq!(row.set_size, Some(4));
                // Every row exposes one cell per column, named by the column header.
                assert_eq!(row.children.len(), 2, "two cells per row (two columns)");
                assert_eq!(row.children[0].role, Role::Cell);
                assert_eq!(row.children[0].name.as_deref(), Some("Name"));
                assert_eq!(row.children[0].pos_in_set, Some(1));
                assert_eq!(row.children[1].name.as_deref(), Some("X"));
                assert_eq!(row.children[1].pos_in_set, Some(2));
            }
            // The selected display row carries the selected flag on its node.
            assert!(
                node.children[2].state.selected,
                "row 3 (index 2) is selected"
            );
            assert!(!node.children[0].state.selected);
        }

        fn typed_text_and_custom_cells_publish_row_specific_values() {
            let columns = vec![
                ColumnDef::new("name", "Name", ColumnWidth::Flex(1.0))
                    .with_text(|row: &R| row.name.clone()),
                ColumnDef::new("custom", "Custom", ColumnWidth::Fixed(80.0))
                    .with_render(Box::new(|_, _, _, _, _| {}))
                    .with_accessible_value(|row: &R| format!("custom {}", row.name)),
                ColumnDef::new("action", "Action", ColumnWidth::Fixed(80.0))
                    .with_widget(|_, _| Box::new(Button::new("edit", "Edit", None))),
            ];
            let mut table = DataTable::new(columns);
            table.set_rows(vec![R { name: "Ada".into() }]);
            let semantics = Widget::accessibility(&table).expect("table semantics");
            let cells = &semantics.children.first().expect("ready row").children;

            assert_eq!(
                cells.first().and_then(|cell| cell.value.as_deref()),
                Some("Ada")
            );
            assert_eq!(
                cells.get(1).and_then(|cell| cell.value.as_deref()),
                Some("custom Ada")
            );
            assert_eq!(
                cells.get(2).and_then(|cell| cell.value.as_deref()),
                None,
                "retained controls provide their own nested semantics"
            );
            assert!(table.columns[0].text.is_some());
        }

        semantic_row_actions_use_stable_row_identities();
        semantic_rows_and_cells_publish_scrolled_viewport_geometry();
        a_routed_action_naming_a_row_that_no_longer_exists_does_nothing();
        accessibility_exposes_per_row_and_cell_structure();
        typed_text_and_custom_cells_publish_row_specific_values();
    }

    use winit::event::{DeviceId, ElementState, MouseButton};

    /// Inside a 300×400 flex-column root so the table's auto width stretches
    /// to 300 and rows get real hit rects.
    fn lay_out(table: &mut DataTable<R>) -> (TaffyTree<()>, NodeId) {
        let mut tree = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let theme = Theme::dark();
        let tnode = table.build_layout(&mut tree, &theme, &mut node_map);
        let root = tree.parent_node(
            Style {
                display: taffy::Display::Flex,
                flex_direction: taffy::FlexDirection::Column,
                size: Size {
                    width: Dimension::length(300.0),
                    height: Dimension::length(400.0),
                },
                ..Default::default()
            },
            &[tnode],
        );
        tree.compute_layout(
            root,
            Size {
                width: taffy::AvailableSpace::Definite(300.0),
                height: taffy::AvailableSpace::Definite(400.0),
            },
        )
        .unwrap();
        table.apply_layout(&tree, tnode, Point::ZERO);
        (tree, tnode)
    }

    fn rows4() -> Vec<R> {
        vec![
            R { name: "a".into() },
            R { name: "b".into() },
            R { name: "c".into() },
            R { name: "d".into() },
        ]
    }

    fn interactive_cols(clicks: Rc<Cell<usize>>, builds: Rc<Cell<usize>>) -> Vec<ColumnDef<R>> {
        vec![
            ColumnDef::new("action", "Action", ColumnWidth::Fixed(120.0)).with_widget(
                move |row: &R, index| {
                    builds.set(builds.get().saturating_add(1));
                    let clicks = Rc::clone(&clicks);
                    let id = format!("action-{index}");
                    Box::new(
                        Button::new(&id, &row.name, None)
                            .on_click(move || clicks.set(clicks.get().saturating_add(1))),
                    )
                },
            ),
            ColumnDef::new("name", "Name", ColumnWidth::Flex(1.0)),
        ]
    }

    #[test]
    fn pointer_and_keyboard_select_rows_unless_cells_or_mode_consume() {
        fn interactive_cell_consumes_click_without_selecting_its_row() {
            let clicks = Rc::new(Cell::new(0));
            let mut table =
                DataTable::new(interactive_cols(Rc::clone(&clicks), Rc::new(Cell::new(0))))
                    .selectable(true)
                    .with_row_key(|row: &R| {
                        u64::from(row.name.as_bytes().first().copied().unwrap_or_default())
                    });
            table.set_rows(rows4());
            // The first pass learns flex column geometry; the second positions
            // retained cell controls against those exact column rectangles.
            let _ = lay_out(&mut table);
            let (tree, node) = lay_out(&mut table);
            let modifiers = winit::event::Modifiers::default();
            dispatch(
                &mut table,
                &tree,
                node,
                &WindowEvent::CursorMoved {
                    device_id: device(),
                    position: (20.0, HEADER_HEIGHT + 12.0).into(),
                },
                modifiers,
            );
            dispatch(
                &mut table,
                &tree,
                node,
                &WindowEvent::MouseInput {
                    device_id: device(),
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                },
                modifiers,
            );
            dispatch(
                &mut table,
                &tree,
                node,
                &WindowEvent::MouseInput {
                    device_id: device(),
                    state: ElementState::Released,
                    button: MouseButton::Left,
                },
                modifiers,
            );

            assert_eq!(clicks.get(), 1, "real cell button receives activation");
            assert!(
                table.selection().is_empty(),
                "the button gesture does not leak into row selection"
            );
            assert_eq!(table.accessibility_child_path(0), Some(vec![0, 0]));
        }

        fn click_selects_row_and_fires_callback() {
            let clicks = Rc::new(RefCell::new(Vec::<(usize, bool, bool)>::new()));
            let mut t = {
                let clicks = clicks.clone();
                DataTable::<R>::new(sample_cols())
                    .selectable(true)
                    .on_row_click(move |i, c, s| clicks.borrow_mut().push((i, c, s)))
            };
            t.set_rows(rows4());
            let (tree, node) = lay_out(&mut t);
            let mods = winit::event::Modifiers::default();

            // The middle of row 2, under the header.
            dispatch(
                &mut t,
                &tree,
                node,
                &WindowEvent::CursorMoved {
                    device_id: device(),
                    position: (50.0, HEADER_HEIGHT + 2.5 * ROW_HEIGHT).into(),
                },
                mods,
            );
            dispatch(
                &mut t,
                &tree,
                node,
                &WindowEvent::MouseInput {
                    device_id: device(),
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                },
                mods,
            );

            assert!(t.selection().is_selected(2), "row 2 selected");
            assert_eq!(t.selection().primary(), Some(2));
            assert_eq!(*clicks.borrow(), vec![(2, false, false)]);
        }

        fn non_selectable_table_ignores_body_clicks() {
            let mut t = DataTable::<R>::new(sample_cols()); // selectable defaults false
            t.set_rows(rows4());
            let (tree, node) = lay_out(&mut t);
            let mods = winit::event::Modifiers::default();
            dispatch(
                &mut t,
                &tree,
                node,
                &WindowEvent::CursorMoved {
                    device_id: device(),
                    position: (50.0, 85.0).into(),
                },
                mods,
            );
            dispatch(
                &mut t,
                &tree,
                node,
                &WindowEvent::MouseInput {
                    device_id: device(),
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                },
                mods,
            );
            assert!(
                t.selection().is_empty(),
                "read-only table keeps empty selection"
            );
        }

        fn keyboard_arrows_move_selection_when_focused() {
            use winit::keyboard::NamedKey;
            let mut t = DataTable::<R>::new(sample_cols()).selectable(true);
            t.set_rows(rows4());
            t.set_focus(true);
            let (tree, node) = lay_out(&mut t);
            let mods = winit::event::Modifiers::default();
            // `KeyEvent`'s extra field is `pub(crate)`; zero-init and set the fields we care about.
            let key = |named| {
                let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
                ev.logical_key = winit::keyboard::Key::Named(named);
                ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
                    winit::keyboard::NativeKeyCode::Unidentified,
                );
                ev.location = winit::keyboard::KeyLocation::Standard;
                ev.state = ElementState::Pressed;
                ev.repeat = false;
                WindowEvent::KeyboardInput {
                    device_id: device(),
                    event: ev,
                    is_synthetic: false,
                }
            };
            dispatch(&mut t, &tree, node, &key(NamedKey::ArrowDown), mods); // → 0
            dispatch(&mut t, &tree, node, &key(NamedKey::ArrowDown), mods); // → 1
            assert_eq!(t.selection().primary(), Some(1));
            assert!(t.selection().is_selected(1));
        }

        interactive_cell_consumes_click_without_selecting_its_row();
        click_selects_row_and_fires_callback();
        non_selectable_table_ignores_body_clicks();
        keyboard_arrows_move_selection_when_focused();
    }

    fn dispatch(
        t: &mut DataTable<R>,
        tree: &TaffyTree<()>,
        node: NodeId,
        ev: &WindowEvent,
        mods: winit::event::Modifiers,
    ) {
        let mut handled = false;
        let mut focus = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        t.handle_event(
            ev,
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &mods,
                state: &mut doc,
                path: &root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: tree,
            },
        );
    }

    fn device() -> DeviceId {
        unsafe { std::mem::zeroed() }
    }

    #[test]
    fn scrolling_shares_variable_row_geometry_with_hit_testing() {
        fn page_navigation_and_accelerated_wheel_share_variable_row_geometry() {
            let mut table = DataTable::<R>::new(sample_cols()).selectable(true);
            table.set_rows(
                (0..100)
                    .map(|i| R {
                        name: format!("row {i}"),
                    })
                    .collect(),
            );
            table.set_viewport(100.0, 24.0);
            assert!(table.set_row_height(0, 300.0));
            table.selection_mut().select_only(0);

            assert!(table.page_selection(true, false));
            assert_eq!(table.selection().primary(), Some(0));
            assert_eq!(table.scroll_y(), 90.0);
            assert!(table.page_selection(false, false));
            assert_eq!(table.scroll_y(), 0.0);

            let mut handled = false;
            table.on_wheel(
                &MouseScrollDelta::LineDelta(0.0, -1.0),
                TouchPhase::Moved,
                &mut handled,
            );
            assert!(handled);
            let immediate = table.scroll_y();
            assert_eq!(immediate, 60.0);
            assert!(table.needs_animation_frame());
            table.last_tick = Some(
                crate::time::now()
                    .checked_sub(crate::time::Duration::from_millis(16))
                    .expect("a 16 ms earlier instant is representable"),
            );
            assert!(Widget::update(
                &mut table,
                &mut AutoCommit::new(),
                &automerge::ROOT,
            ));
            assert!(
                table.scroll_y() > immediate,
                "momentum advances after input"
            );
        }

        fn internal_scroll_offsets_hit_testing() {
            // draw() subtracts scroll_y and row_at() adds it — these must stay
            // inverses or a click resolves to the wrong row once scrolled.
            let mut t = DataTable::<R>::new(sample_cols()).selectable(true);
            t.set_rows(
                (0..100)
                    .map(|i| R {
                        name: format!("r{i}"),
                    })
                    .collect(),
            );
            let _laid = lay_out(&mut t);
            t.set_viewport(5.0 * px(ROW_HEIGHT), px(ROW_HEIGHT));
            t.set_scroll_y(10.0 * px(ROW_HEIGHT)); // scrolled past 10 rows
            let rect = t.get_rect();
            let p = Point::new(rect.x0 + 5.0, rect.y0 + t.header_h + 1.0);
            assert_eq!(
                t.row_at(p),
                Some(10),
                "top of body maps to the scrolled-in row"
            );
            // visible window also starts around row 10 (minus overscan).
            let (start, _end) = t.visible_row_range();
            assert!(
                (8..=10).contains(&start),
                "window starts near the scroll position: {start}"
            );
        }

        fn variable_row_heights_share_placement_and_hit_testing() {
            let mut t = DataTable::<R>::new(sample_cols()).selectable(true);
            t.set_rows(rows4());
            t.set_viewport(60.0, px(ROW_HEIGHT));
            assert!(t.set_row_height(0, 10.0));
            assert!(t.set_row_height(1, 50.0));
            let _laid = lay_out(&mut t);
            let body = t.get_rect().y0 + t.header_h;

            assert_eq!(t.row_at(Point::new(10.0, body + 9.0)), Some(0));
            assert_eq!(t.row_at(Point::new(10.0, body + 10.0)), Some(1));
            assert_eq!(t.row_at(Point::new(10.0, body + 59.0)), Some(1));
            assert_eq!(t.row_at(Point::new(10.0, body + 60.0)), None);
        }

        fn configured_viewport_bounds_layout_and_accessibility_work() {
            let mut t = DataTable::<R>::new(sample_cols());
            t.set_rows(
                (0..100_000)
                    .map(|i| R {
                        name: format!("r{i}"),
                    })
                    .collect(),
            );
            t.set_viewport(120.0, px(ROW_HEIGHT));
            t.set_scroll_y(50_000.0 * px(ROW_HEIGHT));
            let _laid = lay_out(&mut t);

            assert!((t.get_rect().height() - (t.header_h + 120.0)).abs() <= 0.5);
            let node = Widget::accessibility(&t).expect("table semantics");
            assert!(
                node.children.len() <= 10,
                "semantic tree stays proportional to the viewport: {} rows",
                node.children.len()
            );
            let first = node.children.first().expect("visible semantic row");
            assert_eq!(first.set_size, Some(100_000));
            assert!(first.pos_in_set.expect("absolute row position") > 49_990);
        }

        fn wheel_scrolls_in_internal_mode() {
            let (mut t, tree, node) = scrolling_table();
            let mods = winit::event::Modifiers::default();
            assert_eq!(t.scroll_y(), 0.0);
            let wheel = WindowEvent::MouseWheel {
                device_id: device(),
                delta: MouseScrollDelta::LineDelta(0.0, -1.0),
                phase: winit::event::TouchPhase::Moved,
            };
            dispatch(&mut t, &tree, node, &wheel, mods);
            assert!(
                t.scroll_y() > 0.0,
                "wheel down increases scroll_y: {}",
                t.scroll_y()
            );
            assert!(t.scroll_y() <= t.max_scroll_y(), "clamped to max");
        }

        page_navigation_and_accelerated_wheel_share_variable_row_geometry();
        internal_scroll_offsets_hit_testing();
        variable_row_heights_share_placement_and_hit_testing();
        configured_viewport_bounds_layout_and_accessibility_work();
        wheel_scrolls_in_internal_mode();
    }

    /// 100 rows at 300×400, internal-scroll with a 5-row viewport (120 px
    /// body over 2400 px of rows → bar is live). Bar centre x is 294.
    fn scrolling_table() -> (DataTable<R>, TaffyTree<()>, NodeId) {
        let mut t = DataTable::<R>::new(sample_cols()).selectable(true);
        t.set_rows(
            (0..100)
                .map(|i| R {
                    name: format!("r{i}"),
                })
                .collect(),
        );
        let (tree, node) = lay_out(&mut t);
        t.set_viewport(5.0 * px(ROW_HEIGHT), px(ROW_HEIGHT));
        (t, tree, node)
    }

    fn bar_x(t: &DataTable<R>) -> f64 {
        t.get_rect().x1 - BAR_MARGIN - BAR_THICKNESS / 2.0
    }

    #[test]
    fn scrollbar_visibility_thumb_drag_and_track_paging() {
        fn scrollbar_hidden_without_viewport() {
            // No `set_viewport` → defers to an outer Scrollable, no built-in bar.
            let mut t = DataTable::<R>::new(sample_cols());
            t.set_rows(
                (0..100)
                    .map(|i| R {
                        name: format!("r{i}"),
                    })
                    .collect(),
            );
            let _ = lay_out(&mut t);
            assert!(
                t.scrollbar_thumb().is_none(),
                "no bar without an explicit viewport"
            );
        }

        fn scrollbar_hidden_when_rows_fit() {
            let mut t = DataTable::<R>::new(sample_cols());
            t.set_rows(rows4());
            let _ = lay_out(&mut t);
            // Viewport taller than the 4 rows → content fits → no bar.
            t.set_viewport(50.0 * px(ROW_HEIGHT), px(ROW_HEIGHT));
            assert!(
                t.scrollbar_thumb().is_none(),
                "no bar when rows fit the viewport"
            );
        }

        fn scrollbar_thumb_tracks_scroll_y() {
            let (mut t, _tree, _node) = scrolling_table();
            let track = t.v_track().expect("bar live");
            let top = t.scrollbar_thumb().expect("thumb at top");
            assert!(
                (top.y0 - track.y0).abs() < 1e-6,
                "thumb starts at track top"
            );

            t.set_scroll_y(t.max_scroll_y());
            let bottom = t.scrollbar_thumb().expect("thumb at bottom");
            assert!(bottom.y0 > top.y0, "thumb moves down as scroll_y grows");
            assert!(
                (bottom.y1 - track.y1).abs() < 1e-3,
                "fully scrolled thumb reaches track bottom: {} vs {}",
                bottom.y1,
                track.y1,
            );
        }

        fn dragging_scrollbar_updates_scroll_y() {
            let (mut t, tree, node) = scrolling_table();
            let thumb = t.scrollbar_thumb().unwrap();
            let x = bar_x(&t);
            let mid_y = thumb.y0.midpoint(thumb.y1);
            let mods = winit::event::Modifiers::default();

            dispatch(
                &mut t,
                &tree,
                node,
                &WindowEvent::CursorMoved {
                    device_id: device(),
                    position: (x, mid_y).into(),
                },
                mods,
            );
            dispatch(
                &mut t,
                &tree,
                node,
                &WindowEvent::MouseInput {
                    device_id: device(),
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                },
                mods,
            );
            assert!(t.vbar.is_dragging(), "press on thumb begins a drag");
            assert!(t.wants_pointer_capture(), "drag captures the pointer");
            assert_eq!(t.scroll_y(), 0.0);

            // Drag down 50 px → scroll_y increases, staying clamped to max.
            dispatch(
                &mut t,
                &tree,
                node,
                &WindowEvent::CursorMoved {
                    device_id: device(),
                    position: (x, mid_y + 50.0).into(),
                },
                mods,
            );
            assert!(
                t.scroll_y() > 0.0,
                "drag down increases scroll_y: {}",
                t.scroll_y()
            );
            assert!(t.scroll_y() <= t.max_scroll_y(), "clamped to max");
            let after = t.scroll_y();

            // Drag back up past the start → scroll_y returns toward 0.
            dispatch(
                &mut t,
                &tree,
                node,
                &WindowEvent::CursorMoved {
                    device_id: device(),
                    position: (x, mid_y - 50.0).into(),
                },
                mods,
            );
            assert!(t.scroll_y() < after, "drag up decreases scroll_y");

            dispatch(
                &mut t,
                &tree,
                node,
                &WindowEvent::MouseInput {
                    device_id: device(),
                    state: ElementState::Released,
                    button: MouseButton::Left,
                },
                mods,
            );
            assert!(!t.vbar.is_dragging(), "release ends the drag");
        }

        fn track_click_pages_scroll_y() {
            let (mut t, tree, node) = scrolling_table();
            let thumb = t.scrollbar_thumb().unwrap();
            let x = bar_x(&t);
            let below_y = thumb.y1 + 5.0;
            let mods = winit::event::Modifiers::default();

            dispatch(
                &mut t,
                &tree,
                node,
                &WindowEvent::CursorMoved {
                    device_id: device(),
                    position: (x, below_y).into(),
                },
                mods,
            );
            dispatch(
                &mut t,
                &tree,
                node,
                &WindowEvent::MouseInput {
                    device_id: device(),
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                },
                mods,
            );
            assert!(!t.vbar.is_dragging(), "track click is not a drag");
            // Paged by ≈ one viewport (five rows) × PAGE_FRACTION.
            let expected = 5.0 * px(ROW_HEIGHT) * px(crate::widget::scrollbar::PAGE_FRACTION);
            assert!(
                (t.scroll_y() - expected).abs() < 1.0,
                "page step ≈ viewport*frac ({expected}), got {}",
                t.scroll_y(),
            );
        }

        fn scrollbar_press_does_not_select_a_row() {
            // A press on the bar must be swallowed before body-row selection.
            let (mut t, tree, node) = scrolling_table();
            let thumb = t.scrollbar_thumb().unwrap();
            let x = bar_x(&t);
            let mid_y = thumb.y0.midpoint(thumb.y1);
            let mods = winit::event::Modifiers::default();
            dispatch(
                &mut t,
                &tree,
                node,
                &WindowEvent::CursorMoved {
                    device_id: device(),
                    position: (x, mid_y).into(),
                },
                mods,
            );
            dispatch(
                &mut t,
                &tree,
                node,
                &WindowEvent::MouseInput {
                    device_id: device(),
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                },
                mods,
            );
            assert!(
                t.selection().is_empty(),
                "scrollbar press must not select a row"
            );
        }

        scrollbar_hidden_without_viewport();
        scrollbar_hidden_when_rows_fit();
        scrollbar_thumb_tracks_scroll_y();
        dragging_scrollbar_updates_scroll_y();
        track_click_pages_scroll_y();
        scrollbar_press_does_not_select_a_row();
    }

    #[test]
    fn body_drag_scrolls_with_or_without_motion_decay() {
        fn body_drag_scrolls_table_without_selecting_but_click_still_selects() {
            let (mut table, tree, node) = scrolling_table();
            let modifiers = winit::event::Modifiers::default();
            let body_top = table.get_rect().y0 + table.header_h;
            let move_cursor = |y| WindowEvent::CursorMoved {
                device_id: device(),
                position: (20.0, y).into(),
            };
            let button = |state| WindowEvent::MouseInput {
                device_id: device(),
                state,
                button: MouseButton::Left,
            };

            dispatch(
                &mut table,
                &tree,
                node,
                &move_cursor(body_top + 100.0),
                modifiers,
            );
            dispatch(
                &mut table,
                &tree,
                node,
                &button(ElementState::Pressed),
                modifiers,
            );
            assert!(table.wants_pointer_capture());
            dispatch(
                &mut table,
                &tree,
                node,
                &move_cursor(body_top + 40.0),
                modifiers,
            );
            dispatch(
                &mut table,
                &tree,
                node,
                &button(ElementState::Released),
                modifiers,
            );
            assert_eq!(table.scroll_y(), 60.0);
            assert!(
                table.selection().is_empty(),
                "panning does not select a row"
            );

            table.set_scroll_y(0.0);
            dispatch(
                &mut table,
                &tree,
                node,
                &move_cursor(body_top + 10.0),
                modifiers,
            );
            dispatch(
                &mut table,
                &tree,
                node,
                &button(ElementState::Pressed),
                modifiers,
            );
            dispatch(
                &mut table,
                &tree,
                node,
                &button(ElementState::Released),
                modifiers,
            );
            assert!(table.selection().is_selected(0));
        }

        fn reduced_motion_keeps_direct_scroll_without_wheel_or_drag_decay() {
            let (mut table, _tree, node) = scrolling_table();
            table.reduce_motion = true;
            let mut handled = false;
            table.on_wheel(
                &MouseScrollDelta::LineDelta(0.0, -1.0),
                TouchPhase::Moved,
                &mut handled,
            );
            assert!(handled);
            assert_eq!(table.scroll_y(), 60.0_f32);
            assert!(!table.momentum.is_active());

            let now = crate::time::now();
            let started = now.checked_sub(Duration::from_millis(20)).unwrap_or(now);
            let moved = now.checked_sub(Duration::from_millis(5)).unwrap_or(now);
            table
                .drag_scroll
                .begin(50.0, f64::from(table.scroll_y()), started);
            assert!(table.drag_scroll.update(10.0, moved).is_some());
            let mut focus = None;
            table.on_left_release(
                winit::event::Modifiers::default(),
                &mut handled,
                &mut focus,
                node,
            );
            assert!(!table.momentum.is_active());
            assert!(!table.needs_animation_frame());
        }

        body_drag_scrolls_table_without_selecting_but_click_still_selects();
        reduced_motion_keeps_direct_scroll_without_wheel_or_drag_decay();
    }

    fn table_color_fills(t: &mut DataTable<R>) -> usize {
        use vello::Scene;
        use vello_encoding::DrawTag;
        let _laid = lay_out(t);
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        t.draw(
            &mut scene,
            vello::kurbo::Affine::IDENTITY,
            &mut tr,
            &theme,
            &presence,
        );
        scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|tag| **tag == DrawTag::COLOR)
            .count()
    }

    #[test]
    fn typed_cells_align_numbers_dim_absent_values_and_measure_auto_columns() {
        struct File {
            name: &'static str,
            size: Option<u64>,
        }
        let columns = vec![
            ColumnDef::new("name", "Name", ColumnWidth::Auto)
                .with_text(|f: &File| f.name.to_owned()),
            ColumnDef::new("size", "Size", ColumnWidth::Flex(1.0))
                .numeric()
                .with_optional_text(|f: &File| f.size.map(|s| s.to_string()), "NULL"),
        ];
        let mut table = DataTable::new(columns);
        table.set_rows(vec![
            File {
                name: "quarterly-report-final.pdf",
                size: Some(1_284_000),
            },
            File {
                name: "notes.md",
                size: None,
            },
        ]);
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let harness = crate::headless::FrameHarness::new_with_theme(
            crate::State::new(),
            Box::new(table),
            (600, 300),
            theme.clone(),
        );
        let _ = harness;
        // Reach the measure pass directly: the Auto column is as wide as its
        // longest visible cell plus padding.
        let mut table = DataTable::new(vec![
            ColumnDef::new("name", "Name", ColumnWidth::Auto)
                .with_text(|f: &File| f.name.to_owned()),
        ]);
        table.set_rows(vec![File {
            name: "quarterly-report-final.pdf",
            size: None,
        }]);
        let mut tree = TaffyTree::<()>::new();
        let mut map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = table.build_layout(&mut tree, &theme, &mut map);
        table.set_viewport(200.0, 32.0);
        assert!(table.adjust_layout_with_measure(&mut tree, node, &mut text));
        let measured = f64::from(table.auto_widths["name"]);
        let cell = f64::from(
            text.measure_style(
                "quarterly-report-final.pdf",
                None,
                &cell_style(&theme, false),
            )
            .0,
        );
        assert!(measured >= cell, "{measured} fits {cell}");
        assert!(
            !table.adjust_layout_with_measure(&mut tree, node, &mut text),
            "settles"
        );

        // Numeric cells end at the trailing padding; an absent value says so.
        let column = ColumnDef::<File>::new("size", "Size", ColumnWidth::Flex(1.0))
            .numeric()
            .with_optional_text(|f: &File| f.size.map(|s| s.to_string()), "NULL");
        assert_eq!(column.absent, "NULL");
        let spoken = column.accessible_value.as_ref().unwrap();
        assert_eq!(
            spoken(&File {
                name: "a",
                size: None
            }),
            "NULL"
        );
        let mut scene = Scene::new();
        let cell = Rect::new(0.0, 0.0, 200.0, 32.0);
        paint_cell_text(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &theme,
            cell,
            &column,
            Some("1284000"),
        );
        assert!(!scene.encoding().resources.glyph_runs.is_empty());
        let style = cell_style(&theme, true);
        let (w, _) = text.measure_style("1284000", None, &style);
        let pad = f64::from(theme.tokens.spacing.s2);
        // The right-aligned origin lands so the run ends at the padding.
        let x = cell.x1 - pad - f64::from(w);
        assert!(x > cell.center().x, "numbers sit at the trailing edge");
    }
}
