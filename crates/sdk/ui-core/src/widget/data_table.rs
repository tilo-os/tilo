//! `DataTable` primitive — sortable, resizable, virtualized tabular view.
//!
//! Sort is non-destructive: rows stay in insertion order and an index `view`
//! carries the display order, so [`DataTable::sort_by_with`] can re-apply the
//! remembered comparator whenever local rows change. [`DataTable::from_model`]
//! instead delegates order and paging to [`TableModel`]. Its sparse viewport
//! keeps memory proportional to measured/visible rows even when the logical
//! model has hundreds of millions of entries. Keyed, variable-height row
//! virtualization ([`DataTableCore::set_viewport`]/[`DataTableCore::visible_row_range`])
//! windows paint, hit testing, fetching, scrolling, and accessibility to the
//! same visible rows.

use crate::num::f32_of_f64;
use crate::widget::{AppModel, ModelPath};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;
use taffy::style_helpers::{length, percent};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use tilo_sort::CompactSort as _;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::Color;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent};

use crate::arriving::Arrival;
use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::{TextKind, TextRenderer};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::tokens::colors::Colors;
use crate::virtual_viewport::{
    DragScroller, ScrollAlignment, ScrollMomentum, SparseAnchor, SparseVirtualViewport, WheelStep,
};
use crate::widget::scrollbar::{PressOutcome, ScrollMetrics, Scrollbar};
use crate::widget::{Element, SkeletonShape, TaffyTreeExt, Widget};

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

/// What a cell's row is doing when the cell paints.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CellState {
    pub selected: bool,
    /// The row is under the pointer.
    pub hovered: bool,
}

/// Register one per column via [`ColumnDef::with_text_render`]; columns
/// without one render nothing (header + chrome still draw). Gets the
/// table's transform (the cell rect is in the table's coordinates) and
/// the row's [`CellState`], so a cell can change with its row: its ink
/// when selected, a glyph that shows under the pointer.
pub type CellTextRender<Row> =
    Box<dyn Fn(&Row, &mut Scene, Affine, &mut TextRenderer, Rect, &Theme, CellState) + Send + Sync>;

/// A cell painter: the cell rect is in the table's coordinates, and the
/// transform is the one the table paints with (an outer scroll's offset
/// included).
pub type CellRender<Row> =
    Box<dyn Fn(&Row, &mut Scene, Affine, &Theme, &mut TextRenderer, Rect) + Send + Sync>;

/// How two rows order under a column, for the table's own sorting.
pub type ColumnCompare<Row> = Arc<dyn Fn(&Row, &Row) -> Ordering + Send + Sync>;

/// Which column a table is sorted by and in which direction. Hand the
/// table a [`crate::LocalState`] of it ([`DataTable::with_sort_state`]) and
/// the order a person chose survives the view being built again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableSort {
    pub key: String,
    pub ascending: bool,
}

impl TableSort {
    #[must_use]
    pub fn ascending(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            ascending: true,
        }
    }
}

/// A cell's badge: a short label and the tone it carries (a status, a
/// level). An empty label paints nothing.
pub type CellBadge<Row> = Arc<dyn Fn(&Row) -> (String, crate::widget::Tone) + Send + Sync>;

/// Whether a checkbox column's box is ticked for a row.
pub type CellChecked<Row> = Arc<dyn Fn(&Row) -> bool + Send + Sync>;

/// Builds a real retained widget for one visible cell. The widget is keyed by
/// the row's stable identity and the column key, so it survives sorting and
/// scrolling while that cell remains in the mounted window.
pub type CellWidgetBuilder<Row> = Box<dyn FnMut(&Row, usize) -> Element>;
pub type CellAccessibleValue<Row> = Arc<dyn Fn(&Row) -> String + Send + Sync>;
/// A cell's text, or `None` for an absent value (a SQL `NULL`, an unset
/// field), which the table paints dimmed as the column's absent label.
pub type CellText<Row> = Arc<dyn Fn(&Row) -> Option<String> + Send + Sync>;

/// Where a column's header, text, badges and controls sit across its width.
/// A column follows its [`TextKind`] unless it says otherwise: numbers at the
/// trailing edge, everything else at the leading edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CellAlign {
    #[default]
    Leading,
    /// A short status or a control (a switch, a badge) under its header.
    Center,
    Trailing,
}

impl CellAlign {
    /// The alignment a column of `kind` takes when it names none.
    #[must_use]
    pub fn for_kind(kind: TextKind) -> Self {
        if kind == TextKind::Numeric {
            Self::Trailing
        } else {
            Self::Leading
        }
    }

    /// The x of something `width` wide placed in `cell` with `pad` kept
    /// from the edge it sits against.
    #[must_use]
    pub fn place(self, cell: Rect, width: f64, pad: f64) -> f64 {
        match self {
            Self::Leading => cell.x0 + pad,
            Self::Center => cell.center().x - width / 2.0,
            Self::Trailing => cell.x1 - pad - width,
        }
    }
}

pub struct ColumnDef<Row: ?Sized> {
    pub key: String,
    pub header: String,
    pub width: ColumnWidth,
    /// Smallest readable width before optional columns hide or the table scrolls.
    pub min_width: f32,
    /// Widest a flexible column grows; the other flexible columns take the rest.
    pub max_width: f32,
    pub sortable: bool,
    /// `None` renders an empty cell.
    pub text_render: Option<CellTextRender<Row>>,
    /// No-op by default; set via [`ColumnDef::with_render`].
    pub render_cell: CellRender<Row>,
    /// `None` by default; set via [`ColumnDef::with_widget`].
    pub widget: Option<CellWidgetBuilder<Row>>,
    /// The column's widgets are a row's actions: mounted only for the row
    /// under the pointer (see [`ColumnDef::revealed_on_hover`]).
    pub reveal_on_hover: bool,
    /// Value exposed for ready rows; set directly or by [`ColumnDef::with_text`].
    pub accessible_value: Option<CellAccessibleValue<Row>>,
    /// The cell text the table paints itself (set by [`ColumnDef::with_text`]
    /// and [`ColumnDef::with_optional_text`]).
    pub text: Option<CellText<Row>>,
    /// What the column's text is ([`TextKind`]): a numeric column's cells
    /// and header align to the trailing edge in tabular figures.
    pub kind: TextKind,
    /// What an absent value reads as, dimmed.
    pub absent: String,
    /// How many lines a cell's text may wrap to before it is elided; the
    /// row grows to hold them.
    pub max_lines: usize,
    /// Which columns give way first when the table is narrow: `0` never
    /// hides, higher numbers hide before lower ones.
    pub priority: u8,
    /// A badge the table measures and paints in the cell.
    pub badge: Option<CellBadge<Row>>,
    /// A checkbox column: the box is ticked by this and toggled through the
    /// table's checkbox callback.
    pub checked: Option<CellChecked<Row>>,
    /// `None` follows [`Self::kind`] (see [`CellAlign::for_kind`]).
    pub align: Option<CellAlign>,
    /// How a sortable column orders rows; `None` orders by the column's
    /// text (numerically for a numeric column), else its badge label.
    pub compare: Option<ColumnCompare<Row>>,
    pub(crate) resize_handle_px: f32,
}

impl<Row: 'static> ColumnDef<Row> {
    /// This column reading rows of any type, as [`DataTableCore`] holds
    /// them: each callback takes the row back to the `Row` it was written
    /// for. A table only ever hands a column the rows it was given, so the
    /// fallbacks (an empty cell, nothing painted) never show.
    fn erase(self) -> ColumnDef<AnyRow> {
        fn typed<Row: 'static>(row: &AnyRow) -> Option<&Row> {
            row.downcast_ref::<Row>()
        }
        ColumnDef {
            key: self.key,
            header: self.header,
            width: self.width,
            min_width: self.min_width,
            max_width: self.max_width,
            sortable: self.sortable,
            text_render: self.text_render.map(|render| -> CellTextRender<AnyRow> {
                Box::new(move |row, scene, transform, text, rect, theme, state| {
                    if let Some(row) = typed::<Row>(row) {
                        render(row, scene, transform, text, rect, theme, state);
                    }
                })
            }),
            render_cell: {
                let render = self.render_cell;
                Box::new(move |row, scene, transform, theme, text, rect| {
                    if let Some(row) = typed::<Row>(row) {
                        render(row, scene, transform, theme, text, rect);
                    }
                })
            },
            widget: self.widget.map(|mut build| -> CellWidgetBuilder<AnyRow> {
                Box::new(move |row, index| match typed::<Row>(row) {
                    Some(row) => build(row, index),
                    None => Box::new(crate::widget::Spacer::new()),
                })
            }),
            reveal_on_hover: self.reveal_on_hover,
            accessible_value: self
                .accessible_value
                .map(|value| -> CellAccessibleValue<AnyRow> {
                    Arc::new(move |row| typed::<Row>(row).map(|row| value(row)).unwrap_or_default())
                }),
            text: self.text.map(|text| -> CellText<AnyRow> {
                Arc::new(move |row| typed::<Row>(row).and_then(|row| text(row)))
            }),
            kind: self.kind,
            absent: self.absent,
            max_lines: self.max_lines,
            priority: self.priority,
            badge: self.badge.map(|badge| -> CellBadge<AnyRow> {
                Arc::new(move |row| typed::<Row>(row).map(|row| badge(row)).unwrap_or_default())
            }),
            checked: self.checked.map(|checked| -> CellChecked<AnyRow> {
                Arc::new(move |row| typed::<Row>(row).is_some_and(|row| checked(row)))
            }),
            align: self.align,
            compare: self.compare.map(|compare| -> ColumnCompare<AnyRow> {
                Arc::new(move |a, b| match (typed::<Row>(a), typed::<Row>(b)) {
                    (Some(a), Some(b)) => compare(a, b),
                    _ => Ordering::Equal,
                })
            }),
            resize_handle_px: self.resize_handle_px,
        }
    }
}

impl<Row: ?Sized> ColumnDef<Row> {
    pub fn new(key: impl Into<String>, header: impl Into<String>, width: ColumnWidth) -> Self {
        Self {
            key: key.into(),
            header: header.into(),
            width,
            min_width: 0.0,
            max_width: f32::INFINITY,
            sortable: false,
            text_render: None,
            render_cell: Box::new(|_, _, _, _, _, _| {}),
            widget: None,
            reveal_on_hover: false,
            accessible_value: None,
            text: None,
            kind: TextKind::Prose,
            absent: String::from("—"),
            max_lines: 1,
            priority: 0,
            badge: None,
            checked: None,
            align: None,
            compare: None,
            resize_handle_px: 6.0,
        }
    }

    /// A column of checkboxes (an enabled header, a selected file): the table
    /// paints the box, toggles it with a pointer or Space, and reports the
    /// row through [`DataTable::on_check`].
    pub fn checkbox(
        key: impl Into<String>,
        header: impl Into<String>,
        checked: impl Fn(&Row) -> bool + Send + Sync + 'static,
    ) -> Self
    where
        Row: 'static,
    {
        let checked: CellChecked<Row> = Arc::new(checked);
        let spoken = Arc::clone(&checked);
        let mut column = Self::new(key, header, ColumnWidth::Auto);
        column.checked = Some(checked);
        column.align = Some(CellAlign::Center);
        column.accessible_value = Some(Arc::new(move |row| {
            if spoken(row) {
                "Checked"
            } else {
                "Not checked"
            }
            .to_owned()
        }));
        column
    }

    /// Let the cell text wrap to `max_lines` lines before it is elided (a
    /// message, a description); the row grows to hold them.
    #[must_use]
    pub fn wrapping(mut self, max_lines: usize) -> Self {
        self.max_lines = max_lines.max(1);
        self
    }

    /// Keep a readable cell width while flexible columns share extra room.
    /// If the remaining columns cannot fit their minima, the table scrolls
    /// horizontally instead of squeezing their text or controls.
    #[must_use]
    pub fn min_width(mut self, width: f32) -> Self {
        self.min_width = if width.is_finite() {
            width.max(0.0)
        } else {
            0.0
        };
        self
    }

    /// Keep a flexible column from growing past `width` (a key beside its
    /// value); the room it leaves goes to the other flexible columns.
    #[must_use]
    pub fn max_width(mut self, width: f32) -> Self {
        self.max_width = if width.is_nan() {
            f32::INFINITY
        } else {
            width.max(0.0)
        };
        self
    }

    /// Hide this column when the table is too narrow for every column:
    /// higher priorities give way first; `0` (the default) never hides.
    #[must_use]
    pub fn priority(mut self, priority: u8) -> Self {
        self.priority = priority;
        self
    }

    /// A badge in each cell (a level, a status), measured and painted by the
    /// table: `badge(row)` gives its label and tone; an empty label paints
    /// none. The label is the cell's accessible value unless the column has
    /// text.
    #[must_use]
    pub fn with_badge(
        mut self,
        badge: impl Fn(&Row) -> (String, crate::widget::Tone) + Send + Sync + 'static,
    ) -> Self
    where
        Row: 'static,
    {
        let badge: CellBadge<Row> = Arc::new(badge);
        if self.accessible_value.is_none() {
            let spoken = Arc::clone(&badge);
            self.accessible_value = Some(Arc::new(move |row| spoken(row).0));
        }
        self.badge = Some(badge);
        self
    }

    /// Say what the column's values are: [`TextKind::Numeric`] for counts,
    /// sizes, prices and times (cells and header then align to the trailing
    /// edge), [`TextKind::Identifier`] for names people must not confuse,
    /// [`TextKind::Secret`] for keys and hashes.
    #[must_use]
    pub fn with_kind(mut self, kind: TextKind) -> Self {
        self.kind = kind;
        self
    }

    /// Place the column's header and cells: [`CellAlign::Center`] for a
    /// column of switches or short statuses that stand under their header.
    #[must_use]
    pub fn with_align(mut self, align: CellAlign) -> Self {
        self.align = Some(align);
        self
    }

    /// Where the column's header and cells sit.
    #[must_use]
    pub fn alignment(&self) -> CellAlign {
        self.align.unwrap_or_else(|| CellAlign::for_kind(self.kind))
    }

    #[must_use]
    pub fn sortable(mut self, v: bool) -> Self {
        self.sortable = v;
        self
    }

    /// A sortable column that orders rows by `compare` rather than by its
    /// text.
    #[must_use]
    pub fn sorted_by(
        mut self,
        compare: impl Fn(&Row, &Row) -> Ordering + Send + Sync + 'static,
    ) -> Self {
        self.sortable = true;
        self.compare = Some(Arc::new(compare));
        self
    }

    /// How the table orders rows when sorted by this column.
    fn comparator(&self) -> Option<ColumnCompare<Row>>
    where
        Row: 'static,
    {
        if let Some(compare) = &self.compare {
            return Some(Arc::clone(compare));
        }
        if let Some(text) = &self.text {
            let text = Arc::clone(text);
            let numeric = self.kind == TextKind::Numeric;
            return Some(Arc::new(move |a, b| {
                compare_text(text(a).as_deref(), text(b).as_deref(), numeric)
            }));
        }
        let badge = Arc::clone(self.badge.as_ref()?);
        Some(Arc::new(move |a, b| {
            compare_text(Some(&badge(a).0), Some(&badge(b).0), false)
        }))
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

    /// Make this column's widgets a row's actions: they are mounted only for
    /// the row under the pointer, or for the selected row where nothing
    /// hovers (touch). At rest the column paints what
    /// [`Self::with_text_render`] draws: a row's quiet state (a liked mark,
    /// a progress figure), with the controls taking its place on approach.
    #[must_use]
    pub fn revealed_on_hover(mut self) -> Self {
        self.reveal_on_hover = true;
        self
    }
}

pub type SortCmp<Row> = Box<dyn Fn(&Row, &Row) -> Ordering + Send + Sync>;

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
    type Row: ?Sized;

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

    /// Where row `index`'s data stands: resident, on its way (saying where
    /// it comes from), or failed with the reason. A pending row keeps its
    /// geometry and stable key, and the table paints its placeholder
    /// ([`crate::widget::ArrivingRows`]); completing a fetch disturbs
    /// neither the scroll anchor nor the selection.
    fn row_state(&self, index: usize) -> Arrival<&Self::Row, &str>;

    /// Whether failed row `index` can be asked for again: pressing it then
    /// calls [`Self::retry`].
    fn can_retry(&self, _index: usize) -> bool {
        false
    }

    /// Ask again for row `index` after it failed.
    fn retry(&mut self, _index: usize) {}

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

/// `(row, ctrl, shift)`, `row` an index into the rows the host set.
type RowClick = Box<dyn FnMut(usize, bool, bool)>;

/// Absent values sort last; numbers by value; text without regard to case.
fn compare_text(a: Option<&str>, b: Option<&str>, numeric: bool) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(a), Some(b)) => {
            let number = |text: &str| text.trim().replace(',', "").parse::<f64>().ok();
            if numeric && let (Some(x), Some(y)) = (number(a), number(b)) {
                return x.total_cmp(&y);
            }
            a.to_lowercase()
                .cmp(&b.to_lowercase())
                .then_with(|| a.cmp(b))
        }
    }
}

/// So row changes can re-apply the current ordering.
struct SortState<Row: ?Sized> {
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

/// `(display_index, point, row_rect)`.
type RowContext = Box<dyn FnMut(usize, Point, Rect)>;

/// A table whose rows are a flattened tree (a process tree, a folder
/// listing): each row's depth, whether it has children and whether they
/// show; the host owns the flattening and folds rows in `on_toggle`.
struct TreeRows<Row: ?Sized> {
    depth: Box<dyn Fn(&Row) -> usize>,
    has_children: Box<dyn Fn(&Row) -> bool>,
    expanded: Box<dyn Fn(&Row) -> bool>,
    on_toggle: Box<dyn FnMut(usize, bool)>,
}

/// The blank row after the last one that adds a row (a new header, a new
/// parameter).
struct AddRow {
    label: String,
    on_add: Box<dyn FnMut()>,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "focus, selection, scrolling, and invalidation are independent table states"
)]
pub struct DataTableCore {
    test_id: String,
    accessible_name: Option<String>,
    columns: Vec<ColumnDef<AnyRow>>,
    rows: Vec<Box<AnyRow>>,
    /// Present for sparse/paged tables. Vec-backed tables keep this `None`
    /// and retain their local sorting API.
    model: Option<Box<dyn TableModel<Row = AnyRow>>>,
    /// Indices into `rows` in display order — sort operates on this so the
    /// original insertion order stays untouched.
    view: Vec<usize>,
    sort: Option<SortState<AnyRow>>,
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
    row_key: Option<RowKey<AnyRow>>,
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
    /// `(row, ctrl, shift)`, `row` an index into the rows the host set
    /// whatever their sorted order, so a stateless host (rebuilt each frame)
    /// can own the selection.
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
    /// The motion toward a row [`Self::reveal_row`] was asked to show.
    scroll_to: crate::motion::ScrollTo,
    /// Device pixels per logical pixel where the table last handled input.
    device_scale: f64,
    /// The motion the theme allows, sampled by the latest draw.
    motion: crate::tokens::EffectiveMotion,
    /// The body's opacity in the latest draw and in the frame on screen:
    /// below 1 while rows fade in after a long reveal.
    fade_drawn: f32,
    fade_shown: f32,
    /// What the rows, the bar over them and the chrome painted this frame
    /// and the last: a scroll that only moved the rows is offered to the
    /// host as a copy, and a row that changed repaints alone.
    shown: crate::scroll_copy::PaintedFrames,
    /// A scroll or the pointer brought other cells near since the last
    /// rebuild: mounting them is described by the frame's damage
    /// ([`crate::widget::StructureChange::Described`]).
    remount: bool,
    reduce_motion: bool,
    /// The placeholders of a model's rows on their way, by row key.
    arrivals: crate::widget::ArrivingRows<u64>,
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
    /// Rows a small control tall (`Compact`) rather than a pointer target
    /// and a step (`Regular`).
    compact: bool,
    /// The row height at the current density and theme.
    default_row_h: f64,
    /// `metrics.scrollbar_gutter` and the lift's bleed, from the theme.
    gutter: f64,
    /// The rounded corners of the window this sits in, learned where it
    /// last drew: its bars keep out of them.
    corners: crate::widget::scrollbar::SurfaceCorners,
    bleed: f64,
    /// Dragging the body scrolls it (touch always does).
    drag_scroll_enabled: bool,
    on_row_context: Option<RowContext>,
    on_row_drag_begin: Option<RowContext>,
    /// The row a press landed on and where, until it becomes a drag.
    row_press: Option<(usize, Point)>,
    /// A plain press on a row of a selection of several, in a table whose
    /// rows can be dragged: the selection stays for the drag, and the press
    /// is a click on release only if no drag began.
    deferred_click: Option<usize>,
    /// Horizontal scroll: how far the columns are scrolled, and how wide
    /// they are together.
    h_offset: f64,
    /// The gutter kept under the rows for the sideways scroll bar while the
    /// columns overflow, so the bar never covers the last row.
    h_reserve: f64,
    content_w: f64,
    hbar: Scrollbar,
    /// The table takes the height its parent gives it and scrolls inside.
    fill_height: bool,
    /// Columns hidden for room, by priority.
    hidden: Vec<bool>,
    tree: Option<TreeRows<AnyRow>>,
    /// A sort shown on a header without the table reordering its rows.
    sort_indicator: Option<(String, bool)>,
    /// Where the order a person chose is kept across rebuilds.
    sort_state: Option<crate::LocalState<Option<TableSort>>>,
    add_row: Option<AddRow>,
    /// `(display_index, checked)` when a checkbox cell is toggled.
    on_check: Option<Box<dyn FnMut(usize, bool)>>,
    /// The display row under the pointer.
    hover_row: Option<usize>,
    /// No header band: the table continues one headed above it, or has a
    /// single self-evident column.
    headerless: bool,
    /// The rows are windowed by a scroll the host owns
    /// ([`Self::host_scrolled`]).
    host_scrolled: bool,
    /// Shown rows on their way after a sort or a change the person made
    /// (`motion::ListChange`), kept across rebuilds.
    moving: row_motion::Moving,
}

/// A row whose type the table no longer names: what [`DataTableCore`] holds
/// and its columns read. [`DataTable`] converts at its edges.
pub type AnyRow = dyn std::any::Any;

/// A table of `Row`s: the [`DataTableCore`] that does the work (everything a
/// table does, compiled once into the toolkit for rows of any type) and the
/// row type its callers read and write.
///
/// The table's behaviour used to be generic over `Row`, so every app compiled
/// it again for each row type it showed (40 to 100 KB of text per app); now
/// only the conversions here are, and every method that does not name a row
/// is the core's, reached through `Deref`.
pub struct DataTable<Row> {
    core: DataTableCore,
    row: std::marker::PhantomData<fn() -> Row>,
}

impl<Row> std::ops::Deref for DataTable<Row> {
    type Target = DataTableCore;

    fn deref(&self) -> &DataTableCore {
        &self.core
    }
}

impl<Row> std::ops::DerefMut for DataTable<Row> {
    fn deref_mut(&mut self) -> &mut DataTableCore {
        &mut self.core
    }
}

impl<Row: 'static> DataTable<Row> {
    /// Minimum column width in pixels: dragging below this clamps.
    pub const MIN_COL_WIDTH: f32 = DataTableCore::MIN_COL_WIDTH;

    fn of(core: DataTableCore) -> Self {
        Self {
            core,
            row: std::marker::PhantomData,
        }
    }

    /// The row a type-erased row is; every row the core hands back is one
    /// this table was given.
    fn typed(row: &AnyRow) -> Option<&Row> {
        row.downcast_ref::<Row>()
    }

    pub fn new(columns: Vec<ColumnDef<Row>>) -> Self {
        Self::of(DataTableCore::new(
            columns.into_iter().map(ColumnDef::erase).collect(),
        ))
    }

    /// Construct a sparse table backed by an arbitrary model. Geometry and
    /// selection remain proportional to visible/measured rows rather than
    /// `model.len()`.
    pub fn from_model<M>(columns: Vec<ColumnDef<Row>>, model: M) -> Self
    where
        M: TableModel<Row = Row> + 'static,
    {
        Self::of(DataTableCore::from_model(
            columns.into_iter().map(ColumnDef::erase).collect(),
            Box::new(ErasedModel(model)),
        ))
    }

    /// `DataTableCore::tree`, reading this table's rows.
    #[must_use]
    pub fn tree(
        self,
        depth: impl Fn(&Row) -> usize + 'static,
        has_children: impl Fn(&Row) -> bool + 'static,
        expanded: impl Fn(&Row) -> bool + 'static,
        on_toggle: impl FnMut(usize, bool) + 'static,
    ) -> Self {
        Self::of(self.core.tree(
            move |row| Self::typed(row).map_or(0, &depth),
            move |row| Self::typed(row).is_some_and(&has_children),
            move |row| Self::typed(row).is_some_and(&expanded),
            on_toggle,
        ))
    }

    /// `DataTableCore::with_row_key`, reading this table's rows.
    #[must_use]
    pub fn with_row_key<F>(self, key: F) -> Self
    where
        F: Fn(&Row) -> u64 + Send + Sync + 'static,
    {
        Self::of(
            self.core
                .with_row_key(move |row| Self::typed(row).map_or(0, &key)),
        )
    }

    /// Replace the rows, keeping the sort and the selection's rows.
    pub fn set_rows(&mut self, rows: Vec<Row>) {
        self.core.set_rows(
            rows.into_iter()
                .map(|row| Box::new(row) as Box<AnyRow>)
                .collect(),
        );
    }

    /// Rows in current sort order.
    pub fn view_rows(&self) -> Vec<&Row> {
        self.core
            .view_rows()
            .into_iter()
            .filter_map(Self::typed)
            .collect()
    }

    /// `DataTableCore::sort_by_with`, comparing this table's rows.
    pub fn sort_by_with<F>(&mut self, cmp: F, key: &str, ascending: bool)
    where
        F: Fn(&Row, &Row) -> Ordering + Send + Sync + 'static,
    {
        self.core.sort_by_with(
            move |a, b| match (Self::typed(a), Self::typed(b)) {
                (Some(a), Some(b)) => cmp(a, b),
                _ => Ordering::Equal,
            },
            key,
            ascending,
        );
    }

    /// [`DataTableCore::without_header`].
    #[must_use]
    pub fn without_header(self) -> Self {
        Self::of(self.core.without_header())
    }

    /// [`DataTableCore::with_row_height`].
    #[must_use]
    pub fn with_row_height(self, row_height_px: f32) -> Self {
        Self::of(self.core.with_row_height(row_height_px))
    }

    /// [`DataTableCore::host_scrolled`].
    #[must_use]
    pub fn host_scrolled(self) -> Self {
        Self::of(self.core.host_scrolled())
    }

    /// [`DataTableCore::compact`].
    #[must_use]
    pub fn compact(self) -> Self {
        Self::of(self.core.compact())
    }

    /// [`DataTableCore::drag_scroll`].
    #[must_use]
    pub fn drag_scroll(self) -> Self {
        Self::of(self.core.drag_scroll())
    }

    /// [`DataTableCore::on_row_context_menu`].
    #[must_use]
    pub fn on_row_context_menu(self, f: impl FnMut(usize, Point, Rect) + 'static) -> Self {
        Self::of(self.core.on_row_context_menu(f))
    }

    /// [`DataTableCore::on_row_drag_begin`].
    #[must_use]
    pub fn on_row_drag_begin(self, f: impl FnMut(usize, Point, Rect) + 'static) -> Self {
        Self::of(self.core.on_row_drag_begin(f))
    }

    /// [`DataTableCore::fill_height`].
    #[must_use]
    pub fn fill_height(self) -> Self {
        Self::of(self.core.fill_height())
    }

    /// [`DataTableCore::with_sort_state`].
    #[must_use]
    pub fn with_sort_state(self, state: &crate::LocalState<Option<TableSort>>) -> Self {
        Self::of(self.core.with_sort_state(state))
    }

    /// [`DataTableCore::with_add_row`].
    #[must_use]
    pub fn with_add_row(self, label: impl Into<String>, on_add: impl FnMut() + 'static) -> Self {
        Self::of(self.core.with_add_row(label, on_add))
    }

    /// [`DataTableCore::on_check`].
    #[must_use]
    pub fn on_check(self, f: impl FnMut(usize, bool) + 'static) -> Self {
        Self::of(self.core.on_check(f))
    }

    /// [`DataTableCore::ruled`].
    #[must_use]
    pub fn ruled(self) -> Self {
        Self::of(self.core.ruled())
    }

    /// [`DataTableCore::with_id`].
    #[must_use]
    pub fn with_id(self, id: impl Into<String>) -> Self {
        Self::of(self.core.with_id(id))
    }

    /// [`DataTableCore::with_accessible_name`].
    #[must_use]
    pub fn with_accessible_name(self, name: impl Into<String>) -> Self {
        Self::of(self.core.with_accessible_name(name))
    }

    /// [`DataTableCore::selectable`].
    #[must_use]
    pub fn selectable(self, v: bool) -> Self {
        Self::of(self.core.selectable(v))
    }

    /// [`DataTableCore::with_model_selection`].
    #[must_use]
    pub fn with_model_selection(self, selection: crate::selection::KeySelection<u64>) -> Self {
        Self::of(self.core.with_model_selection(selection))
    }

    /// [`DataTableCore::with_selection`].
    #[must_use]
    pub fn with_selection(self, sel: crate::selection::SelectionModel) -> Self {
        Self::of(self.core.with_selection(sel))
    }

    /// [`DataTableCore::with_viewport`].
    #[must_use]
    pub fn with_viewport(self, height_px: f32, row_height_px: f32) -> Self {
        Self::of(self.core.with_viewport(height_px, row_height_px))
    }

    /// [`DataTableCore::on_row_click`].
    #[must_use]
    pub fn on_row_click<F: FnMut(usize, bool, bool) + 'static>(self, f: F) -> Self {
        Self::of(self.core.on_row_click(f))
    }

    /// [`DataTableCore::on_activate`].
    #[must_use]
    pub fn on_activate<F: FnMut(usize) + 'static>(self, f: F) -> Self {
        Self::of(self.core.on_activate(f))
    }

    /// [`DataTableCore::on_header_click`].
    #[must_use]
    pub fn on_header_click<F: FnMut(usize) + 'static>(self, f: F) -> Self {
        Self::of(self.core.on_header_click(f))
    }
}

/// A [`TableModel`] of `Row`s as the core reads models: of rows of any type.
struct ErasedModel<M>(M);

impl<M: TableModel + 'static> TableModel for ErasedModel<M>
where
    M::Row: Sized + 'static,
{
    type Row = AnyRow;

    fn len(&self) -> usize {
        self.0.len()
    }

    fn key_at(&self, index: usize) -> Option<u64> {
        self.0.key_at(index)
    }

    fn index_of(&self, key: u64) -> Option<usize> {
        self.0.index_of(key)
    }

    fn row_state(&self, index: usize) -> Arrival<&AnyRow, &str> {
        self.0.row_state(index).map(|row| -> &AnyRow { row })
    }

    fn can_retry(&self, index: usize) -> bool {
        self.0.can_retry(index)
    }

    fn retry(&mut self, index: usize) {
        self.0.retry(index);
    }

    fn request_range(&mut self, range: Range<usize>) {
        self.0.request_range(range);
    }

    fn poll_changed(&mut self) -> bool {
        self.0.poll_changed()
    }

    fn poll_changes(&mut self) -> Vec<TableModelChange> {
        self.0.poll_changes()
    }

    fn sort_by(&mut self, column_key: &str, ascending: bool) -> bool {
        self.0.sort_by(column_key, ascending)
    }

    fn needs_poll(&self) -> bool {
        self.0.needs_poll()
    }
}

impl DataTableCore {
    /// Minimum column width in pixels — dragging below this clamps.
    pub const MIN_COL_WIDTH: f32 = 40.0;

    pub(crate) fn new(columns: Vec<ColumnDef<AnyRow>>) -> Self {
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
            scroll_to: crate::motion::ScrollTo::new(),
            device_scale: 1.0,
            motion: crate::tokens::EffectiveMotion::default(),
            fade_drawn: 1.0,
            fade_shown: 1.0,
            shown: crate::scroll_copy::PaintedFrames::default(),
            remount: false,
            reduce_motion: false,
            arrivals: crate::widget::ArrivingRows::new(),
            header_h: HEADER_HEIGHT,
            authored_row_height: None,
            row_type: None,
            ruled: false,
            auto_widths: HashMap::new(),
            measure_theme: None,
            compact: false,
            default_row_h: ROW_HEIGHT,
            gutter: f64::from(crate::tokens::Metrics::BASE.scrollbar_gutter),
            corners: crate::widget::scrollbar::SurfaceCorners::default(),
            bleed: f64::from(crate::tokens::Spacing::BASE.s2),
            drag_scroll_enabled: false,
            on_row_context: None,
            on_row_drag_begin: None,
            row_press: None,
            deferred_click: None,
            h_offset: 0.0,
            h_reserve: 0.0,
            content_w: 0.0,
            hbar: Scrollbar::horizontal(),
            fill_height: false,
            hidden: Vec::new(),
            tree: None,
            sort_indicator: None,
            sort_state: None,
            add_row: None,
            on_check: None,
            hover_row: None,
            headerless: false,
            host_scrolled: false,
            moving: row_motion::Moving::default(),
        }
    }

    /// Leave out the header band: for a table that continues one headed
    /// above it, or whose one column needs no name.
    #[must_use]
    pub fn without_header(mut self) -> Self {
        self.headerless = true;
        self.header_h = 0.0;
        self.layout_dirty = true;
        self
    }

    /// The estimated height of a row, for rows taller than one line of text
    /// (a title over its artist) in a table that sets no viewport.
    #[must_use]
    pub fn with_row_height(mut self, row_height_px: f32) -> Self {
        self.authored_row_height = Some(f64::from(row_height_px));
        self.refresh_row_estimate();
        self
    }

    /// The table stands in a page its host scrolls as a whole (a painted
    /// page with a hero above the table and more below it). The host keeps
    /// the table's rect on the part of it in view and tells it which rows
    /// that is with [`Self::set_host_window`]; the table mounts and paints
    /// only those, keeps its header at the top of that part, draws no scroll
    /// bar of its own and leaves the wheel to the host.
    #[must_use]
    pub fn host_scrolled(mut self) -> Self {
        self.host_scrolled = true;
        self
    }

    /// For a [`Self::host_scrolled`] table: `body_height` of rows is in
    /// view, `offset` below the first row's top.
    pub fn set_host_window(&mut self, offset: f64, body_height: f64) {
        let body_height = body_height.max(1.0);
        if (self.viewport.viewport() - body_height).abs() > 0.5 {
            self.viewport.set_viewport(body_height);
            self.viewport
                .set_overscan(self.viewport.estimated_height() * 2.0);
            self.visible_window_changed();
        }
        if (self.viewport.offset() - offset.max(0.0)).abs() > f64::EPSILON {
            self.momentum.stop();
            self.set_scroll_offset(offset.max(0.0));
        }
    }

    /// The height of the header and every row together: what a
    /// [`Self::host_scrolled`] table takes of its host's page.
    #[must_use]
    pub fn content_height(&self) -> f64 {
        self.header_h + self.viewport.content_extent()
    }

    /// The row under the pointer, as an index into the rows the host set.
    #[must_use]
    pub fn hovered_row(&self) -> Option<usize> {
        self.hover_row
            .filter(|row| *row < self.display_len())
            .map(|row| self.source_row(row))
    }

    /// Whether a press on a row does something, so the row lifts under the
    /// pointer as a list row does.
    fn rows_pressable(&self) -> bool {
        self.selectable || self.on_row_click.is_some() || self.on_activate.is_some()
    }

    /// The row whose revealed-on-hover cells are mounted: the one under the
    /// pointer, or the selected one where nothing hovers.
    fn revealed_row(&self) -> Option<usize> {
        if self.interaction.supports_hover() {
            self.hover_row
        } else {
            self.primary_index()
        }
    }

    fn reveals_on_hover(&self) -> bool {
        self.columns
            .iter()
            .any(|column| column.reveal_on_hover && column.widget.is_some())
    }

    /// Move the pointer in the table's logical coordinate space.
    /// Embedded hosts can stage hover without constructing document state;
    /// window input uses this same pointer-tracking path.
    pub fn set_pointer_position(&mut self, point: Point) -> bool {
        let mut handled = false;
        self.on_cursor_moved(point, &mut handled);
        handled
    }

    /// Track the row under `pt`; a change remounts the revealed cells.
    fn track_hover(&mut self, pt: Option<Point>) {
        let body_bottom = self.rect.y0 + self.header_h + self.body_viewport_h();
        let row = pt
            .filter(|pt| pt.y < body_bottom)
            .and_then(|pt| self.row_at(pt));
        if row != self.hover_row {
            self.hover_row = row;
            if self.reveals_on_hover() {
                self.remount = true;
                self.layout_dirty = true;
            }
        }
    }

    /// Rows a small control tall (32 at the default scale) instead of 40:
    /// a dense grid (query results, logs).
    #[must_use]
    pub fn compact(mut self) -> Self {
        self.compact = true;
        self
    }

    /// Let a pointer drag on the body scroll it, as a finger does. Off by
    /// default for a mouse, so a press selects at once and a drag can
    /// begin a row drag ([`Self::on_row_drag_begin`]).
    #[must_use]
    pub fn drag_scroll(mut self) -> Self {
        self.drag_scroll_enabled = true;
        self
    }

    /// Run `f(row, point, row_rect)` when a row is right-clicked or asked
    /// for its menu by assistive technology. The row is selected first,
    /// unless it already is.
    #[must_use]
    pub fn on_row_context_menu(mut self, f: impl FnMut(usize, Point, Rect) + 'static) -> Self {
        self.on_row_context = Some(Box::new(f));
        self
    }

    /// Run `f(row, point, row_rect)` when a pointer drag starts on a row,
    /// to begin a drag and drop. Use [`Self::row_rect`] and
    /// [`Self::row_at_point`] to find drop targets.
    #[must_use]
    pub fn on_row_drag_begin(mut self, f: impl FnMut(usize, Point, Rect) + 'static) -> Self {
        self.on_row_drag_begin = Some(Box::new(f));
        self
    }

    /// Take the height the parent gives the table and scroll the rows
    /// inside it, with the shared scroll bar.
    #[must_use]
    pub fn fill_height(mut self) -> Self {
        self.fill_height = true;
        self.layout_dirty = true;
        self
    }

    /// The header band's height at the current text scale.
    pub fn header_height(&self) -> f64 {
        self.header_h
    }

    /// Show the rows as a flattened tree: each row indented by `depth(row)`,
    /// with a chevron when `has_children(row)` that shows `expanded(row)`.
    /// Pressing the chevron, or Right and Left on the selected row, calls
    /// `on_toggle(row, expand)`; the host refolds its rows.
    #[must_use]
    pub(crate) fn tree(
        mut self,
        depth: impl Fn(&AnyRow) -> usize + 'static,
        has_children: impl Fn(&AnyRow) -> bool + 'static,
        expanded: impl Fn(&AnyRow) -> bool + 'static,
        on_toggle: impl FnMut(usize, bool) + 'static,
    ) -> Self {
        self.tree = Some(TreeRows {
            depth: Box::new(depth),
            has_children: Box::new(has_children),
            expanded: Box::new(expanded),
            on_toggle: Box::new(on_toggle),
        });
        self
    }

    /// Show `key`'s header as sorted without reordering the rows: the host
    /// sorted them itself.
    pub fn set_sort_indicator(&mut self, key: impl Into<String>, ascending: bool) {
        self.sort_indicator = Some((key.into(), ascending));
    }

    /// Sort by the column `state` names, and keep the order a person picks
    /// from the headers in `state`, so it survives the view being built
    /// again. Rows and their callbacks keep the host's indices.
    #[must_use]
    pub fn with_sort_state(mut self, state: &crate::LocalState<Option<TableSort>>) -> Self {
        if let Some(sort) = state.with(Clone::clone) {
            self.sort_by_column(&sort.key, sort.ascending);
        }
        self.sort_state = Some(state.clone());
        self
    }

    /// Sort by the column keyed `key` with its own comparator; `false` when
    /// there is no such sortable column.
    fn sort_by_column(&mut self, key: &str, ascending: bool) -> bool {
        let Some(compare) = self
            .columns
            .iter()
            .find(|column| column.key == key && column.sortable)
            .and_then(ColumnDef::comparator)
        else {
            return false;
        };
        self.sort_by_with(move |a, b| compare(a, b), key, ascending);
        true
    }

    /// A blank row after the last one that adds a row when pressed (a new
    /// header, a new parameter); `label` is its placeholder.
    #[must_use]
    pub fn with_add_row(
        mut self,
        label: impl Into<String>,
        on_add: impl FnMut() + 'static,
    ) -> Self {
        self.add_row = Some(AddRow {
            label: label.into(),
            on_add: Box::new(on_add),
        });
        self
    }

    /// Run `f(row, checked)` when a checkbox column's box is toggled.
    #[must_use]
    pub fn on_check(mut self, f: impl FnMut(usize, bool) + 'static) -> Self {
        self.on_check = Some(Box::new(f));
        self
    }

    /// The rows, for editing in place; follow with [`Self::apply_changes`].
    #[cfg(test)]
    pub(crate) fn rows_mut(&mut self) -> &mut Vec<Box<AnyRow>> {
        &mut self.rows
    }

    /// Reconcile edits made through `Self::rows_mut`: cell controls stay
    /// mounted by row key (so a field being typed into keeps its focus),
    /// and only the rows named in [`TableModelChange::RowsChanged`] rebuild
    /// theirs. Give the table a `Self::with_row_key` for keys that
    /// survive inserts and removals.
    pub fn apply_changes(&mut self, changes: &[TableModelChange]) {
        if self.model.is_some() {
            self.notify_model_changes(changes);
            return;
        }
        self.capture_row_motion(changes);
        let anchor = self.capture_view_anchor();
        self.rebuild_row_keys();
        self.unsort_view();
        if changes
            .iter()
            .any(|change| matches!(change, TableModelChange::Reset))
        {
            self.mounted_cells.clear();
        } else {
            self.mounted_cells.retain(|cell| {
                !changes.iter().any(|change| match change {
                    TableModelChange::RowsChanged { keys } => keys.contains(&cell.row_key.value),
                    _ => false,
                })
            });
        }
        self.reapply_sort_with_anchor(anchor.as_ref());
        self.structure_dirty = true;
        self.layout_dirty = true;
    }

    /// The rect of display row `index` (the lift's rect), for drop targets.
    pub fn row_rect(&self, index: usize) -> Option<Rect> {
        let top = self.viewport.item_view_top(index)?;
        let height = self.viewport.item_height(index)?;
        let band = self.columns_band();
        let y0 = self.rect.y0 + self.header_h + top;
        Some(Rect::new(band.x0, y0, band.x1, y0 + height))
    }

    pub fn row_at_point(&self, point: Point) -> Option<usize> {
        self.row_at(point)
    }

    /// Scroll display row `index` into view, by the nearest edge. A reveal
    /// the person's hand or key just asked for (a search hit, a selection
    /// made elsewhere) moves there ([`crate::motion::ScrollTo`]): a short
    /// jump glides, a long one fades in at the row; one nobody asked for
    /// lands at once.
    pub fn reveal_row(&mut self, index: usize) {
        if index >= self.display_len() {
            return;
        }
        self.momentum.stop();
        let now = crate::motion::frame_now();
        let drawn = self.viewport.offset();
        if let Some(target) = self.scroll_to.target() {
            self.viewport.set_offset(target.y);
        }
        self.viewport
            .scroll_to_index(index, ScrollAlignment::Nearest);
        let target = self.viewport.offset();
        self.viewport.set_offset(drawn);
        let jump = self.scroll_to.go(
            vello::kurbo::Vec2::new(0.0, drawn),
            vello::kurbo::Vec2::new(0.0, target),
            vello::kurbo::Vec2::new(self.rect.width(), self.viewport.viewport()),
            self.motion,
            now,
        );
        if jump == crate::motion::ScrollJump::Glide {
            self.follow_scroll_to(now);
        } else if target.to_bits() != drawn.to_bits() {
            self.set_scroll_offset(target);
            self.visible_window_changed();
        }
    }

    /// Follow a reveal in progress to where it is drawn at `now`.
    fn follow_scroll_to(&mut self, now: crate::time::Instant) {
        if let Some(at) = self
            .scroll_to
            .offset_in_device_pixels(now, self.device_scale)
        {
            if at.y.to_bits() != self.viewport.offset().to_bits() {
                self.set_scroll_offset(at.y);
            }
            let _ = self.scroll_to.arrived(now);
        }
    }

    /// A reveal still moves, its rows still fade in, or the frame on screen
    /// shows them faded.
    fn reveal_in_progress(&self) -> bool {
        self.scroll_to.target().is_some()
            || self.scroll_to.is_moving(crate::motion::frame_now())
            || self.fade_drawn < 1.0
            || self.fade_shown < 1.0
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
    pub(crate) fn from_model(
        columns: Vec<ColumnDef<AnyRow>>,
        model: Box<dyn TableModel<Row = AnyRow>>,
    ) -> Self {
        let mut table = Self::new(columns);
        table.model = Some(model);
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

    /// Run `f(row, ctrl, shift)` when a row is clicked. Every row callback
    /// names the row by its index in the rows the host set
    /// (`Self::set_rows`), however the table has sorted them.
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
    pub(crate) fn with_row_key<F>(mut self, key: F) -> Self
    where
        F: Fn(&AnyRow) -> u64 + Send + Sync + 'static,
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

    pub(crate) fn set_rows(&mut self, rows: Vec<Box<AnyRow>>) {
        // Rows a keyed table's host hands in again in another order (a
        // queue the person reordered) move from where they were drawn.
        if self.row_key.is_some() && self.model.is_none() {
            self.capture_reorder_motion();
        }
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
        self.unsort_view();
        self.reapply_sort_with_anchor(anchor.as_ref());
    }

    /// Show the rows in the host's order again, the selection staying on
    /// the rows it names, before the sort is applied afresh.
    fn unsort_view(&mut self) {
        if self.model.is_none() && !self.selection.is_empty() {
            let selected: Vec<usize> = self
                .selection
                .selected()
                .into_iter()
                .map(|display| self.source_row(display))
                .collect();
            let primary = self
                .selection
                .primary()
                .map(|display| self.source_row(display));
            self.selection.replace_indices(selected, primary);
        }
        self.view = (0..self.rows.len()).collect();
    }

    fn rebuild_row_keys(&mut self) {
        let mut occurrences = HashMap::<u64, usize>::new();
        self.row_keys = self
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let value = self.row_key.as_ref().map_or_else(
                    || u64::try_from(index).unwrap_or(u64::MAX),
                    |key| key(&**row),
                );
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
        // A table without a viewport of its own shows every row and never
        // scrolls: there is no first visible row to keep in place.
        let anchor = anchor.filter(|_| self.viewport.viewport() > 0.0);
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

    /// The index in the host's rows of the row shown at `display_index`;
    /// a model-backed table's model orders its own rows.
    fn source_row(&self, display_index: usize) -> usize {
        if self.model.is_some() {
            return display_index;
        }
        self.view
            .get(display_index)
            .copied()
            .unwrap_or(display_index)
    }

    /// Where the host's row `row` is shown.
    fn display_row(&self, row: usize) -> Option<usize> {
        if self.model.is_some() {
            return Some(row);
        }
        self.view.iter().position(|source| *source == row)
    }

    /// Select only the host's row `row` (an index into the rows set with
    /// `Self::set_rows`), wherever sorting shows it.
    pub fn select_row(&mut self, row: usize) {
        if let Some(display) = self.display_row(row) {
            self.select_only_index(display);
        }
    }

    /// The selected rows as indices into the rows the host set, in the
    /// order the table shows them.
    #[must_use]
    pub fn selected_rows(&self) -> Vec<usize> {
        self.selection
            .selected()
            .into_iter()
            .map(|display| self.source_row(display))
            .collect()
    }

    fn display_len(&self) -> usize {
        self.model
            .as_deref()
            .map_or(self.view.len(), TableModel::len)
    }

    fn row_state_at(&self, display_index: usize) -> Option<Arrival<&AnyRow, &str>> {
        if let Some(model) = self.model.as_deref() {
            return (display_index < model.len()).then(|| model.row_state(display_index));
        }
        let source_index = *self.view.get(display_index)?;
        self.rows
            .get(source_index)
            .map(|row| Arrival::Ready(&**row))
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

    pub fn columns(&self) -> &[ColumnDef<AnyRow>] {
        &self.columns
    }

    /// Rows in current sort order.
    pub(crate) fn view_rows(&self) -> Vec<&AnyRow> {
        if self.model.is_some() {
            return self
                .viewport
                .visible_range()
                .filter_map(|index| match self.row_state_at(index) {
                    Some(Arrival::Ready(row)) => Some(row),
                    _ => None,
                })
                .collect();
        }
        self.view
            .iter()
            .filter_map(|&i| self.rows.get(i).map(|row| &**row))
            .collect()
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
        self.capture_row_motion(changes);
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
        self.sort
            .as_ref()
            .map(|s| (s.key.as_str(), s.ascending))
            .or_else(|| {
                self.sort_indicator
                    .as_ref()
                    .map(|(key, ascending)| (key.as_str(), *ascending))
            })
    }

    /// Stores the comparator + key + direction so `set_rows` re-applies the
    /// same ordering automatically. Model-backed tables delegate sorting to
    /// [`TableModel::sort_by`] instead.
    pub(crate) fn sort_by_with<F>(&mut self, cmp: F, key: &str, ascending: bool)
    where
        F: Fn(&AnyRow, &AnyRow) -> Ordering + Send + Sync + 'static,
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

    /// Reuses the comparator already installed via `Self::sort_by_with`;
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
        // The selection is kept by where rows are shown; it names the same
        // rows after they move.
        let selected: Vec<usize> = self
            .selection
            .selected()
            .into_iter()
            .filter_map(|display| self.view.get(display).copied())
            .collect();
        let primary = self
            .selection
            .primary()
            .and_then(|display| self.view.get(display).copied());
        if let Some(s) = &self.sort {
            let rows = &self.rows;
            let cmp = &s.cmp;
            let ascending = s.ascending;
            self.view.compact_sort_by(|&a, &b| {
                let (Some(ra), Some(rb)) = (rows.get(a), rows.get(b)) else {
                    return Ordering::Equal;
                };
                let o = cmp(&**ra, &**rb);
                if ascending { o } else { o.reverse() }
            });
            // Cell controls follow their rows to where they now stand.
            if !self.mounted_cells.is_empty() {
                self.structure_dirty = true;
            }
            self.layout_dirty = true;
        }
        if self.model.is_none() && !selected.is_empty() {
            let mut shown = vec![usize::MAX; self.rows.len()];
            for (display, source) in self.view.iter().enumerate() {
                if let Some(slot) = shown.get_mut(*source) {
                    *slot = display;
                }
            }
            let at = |source: usize| shown.get(source).copied().filter(|d| *d != usize::MAX);
            self.selection.replace_indices(
                selected.iter().filter_map(|source| at(*source)),
                primary.and_then(at),
            );
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
        // Rows draw themselves; only a column of controls mounts cells for
        // the rows the scroll brought near.
        if self.viewport.visible_range() != before
            && self.columns.iter().any(|column| column.widget.is_some())
        {
            self.remount = true;
        }
        self.layout_dirty = true;
        self.last_anchor = self.capture_view_anchor();
        // The rows moved under a pointer that did not.
        self.track_hover(self.last_cursor);
    }

    #[must_use]
    pub fn scroll_y(&self) -> f32 {
        f32_of_f64(self.viewport.offset())
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
        self.header_h = if self.headerless {
            0.0
        } else {
            text_band(
                f64::from(tokens.metrics.min_pointer_target),
                line,
                f64::from(tokens.spacing.s3),
            )
        };
        self.row_type = Some(RowType {
            line,
            default_pad: f64::from(tokens.spacing.rhythm.tight),
            authored_pad: f64::from(tokens.spacing.s1),
        });
        self.default_row_h = table_row_floor(theme, self.compact);
        self.gutter = f64::from(tokens.metrics.scrollbar_gutter);
        self.bleed = crate::chrome::row_bleed(theme);
        self.refresh_row_estimate();
    }

    /// The one place the viewport's estimated row height is set, from the
    /// authored estimate and the label type seen so far (see
    /// [`row_estimate`]). Called when either input changes.
    fn refresh_row_estimate(&mut self) {
        let estimate = row_estimate(self.authored_row_height, self.default_row_h, self.row_type);
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
        let revealed = self.revealed_row();
        let mut desired = Vec::with_capacity(range.len().saturating_mul(widget_columns.len()));
        for row_index in range {
            let Some(row_key) = self.row_identity_at(row_index) else {
                continue;
            };
            for &column_index in &widget_columns {
                let actions = self
                    .columns
                    .get(column_index)
                    .is_some_and(|column| column.reveal_on_hover);
                if actions && revealed != Some(row_index) {
                    continue;
                }
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
                    Arrival::Ready(row) => Some(row),
                    Arrival::Pending { .. } | Arrival::Failed(_) => None,
                }
            } else {
                self.view
                    .get(row_index)
                    .and_then(|source| self.rows.get(*source).map(|row| &**row))
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
            (self.rect.height() - self.header_h - self.h_reserve).max(0.0)
        }
    }

    fn content_body_h(&self) -> f64 {
        self.viewport.content_extent()
    }

    /// `content - viewport`, floored at 0.
    pub fn max_scroll_y(&self) -> f32 {
        f32_of_f64(self.viewport.max_offset())
    }

    fn v_metrics(&self) -> ScrollMetrics {
        ScrollMetrics::new(
            self.content_body_h(),
            self.body_viewport_h(),
            self.viewport.offset(),
        )
    }

    /// Live when an explicit viewport was set and the rows overflow it.
    /// How far past an end fingers pull the rows, as drawn: negative before
    /// the start ([`crate::motion::RubberBand`]).
    fn give(&self) -> f64 {
        self.momentum.give(crate::motion::frame_now())
    }

    fn internal_scroll(&self) -> bool {
        !self.host_scrolled && self.viewport.viewport() > 0.0 && self.v_metrics().overflows()
    }

    /// The band the columns share, by the one content edge (HIG §9.3): the
    /// first column's text starts on the table's leading edge and the last
    /// column's ends on its trailing edge, each cell padded by the bleed, so
    /// the band, and the lift and stripe that fill it, stand the bleed past
    /// the table on both sides. While the scroll bar shows, the band stops
    /// at the bar's gutter, so the bar never covers a column.
    fn columns_band(&self) -> Rect {
        let x0 = self.rect.x0 - self.bleed;
        let x1 = if self.internal_scroll() {
            self.rect.x1 - self.gutter
        } else {
            self.rect.x1 + self.bleed
        };
        Rect::new(x0, self.rect.y0, x1.max(x0), self.rect.y1)
    }

    /// [`Widget::collect_damage`] for the widget `owner` in the tree that
    /// holds this table (the typed table around it), which offers the copy.
    fn collect_damage_of<T: ?Sized>(&self, owner: &T, regions: &mut Vec<Rect>) {
        let bounds = self.paint_bounds();
        if self.fade_drawn < 1.0 || self.fade_shown < 1.0 {
            regions.push(bounds);
            return;
        }
        if self.shown.drawing().is_some() {
            self.shown.damage_within(owner, bounds, regions);
        }
        for cell in &self.mounted_cells {
            cell.widget.collect_damage(regions);
        }
    }

    /// Everything the table paints: its box, and the rows' band that
    /// bleeds past it.
    fn paint_bounds(&self) -> Rect {
        let band = self.columns_band();
        self.rect
            .union(Rect::new(band.x0, self.rect.y0, band.x1, self.rect.y1))
    }

    /// The body's rect: under the header, as tall as the viewport.
    fn body_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0,
            self.rect.y0 + self.header_h,
            self.rect.x1,
            self.rect.y0 + self.header_h + self.body_viewport_h(),
        )
    }

    fn v_track(&self) -> Option<Rect> {
        if !self.internal_scroll() {
            return None;
        }
        let track = self.corners.clear(
            crate::widget::scrollbar::vertical_track(self.body_rect(), self.gutter),
            true,
        );
        if track.height() <= crate::widget::scrollbar::MIN_THUMB_LEN.min(1.0) {
            return None;
        }
        Some(track)
    }

    /// Whether the columns are wider than the band and scroll sideways.
    fn h_overflow(&self) -> bool {
        self.content_w > self.columns_band().width() + 0.5
    }

    fn h_metrics(&self) -> ScrollMetrics {
        ScrollMetrics::new(self.content_w, self.columns_band().width(), self.h_offset)
    }

    fn h_track(&self) -> Option<Rect> {
        if !self.h_overflow() {
            return None;
        }
        // In the gutter kept under the rows.
        let band = self.columns_band();
        let body = self.body_rect();
        let track = crate::widget::scrollbar::horizontal_track(
            Rect::new(
                band.x0,
                body.y1,
                band.x1,
                body.y1 + self.h_reserve.max(self.gutter),
            ),
            self.gutter,
        );
        let track = self.corners.clear(track, false);
        (track.width() > crate::widget::scrollbar::MIN_THUMB_LEN).then_some(track)
    }

    fn set_h_offset(&mut self, offset: f64) {
        let max = (self.content_w - self.columns_band().width()).max(0.0);
        let next = offset.clamp(0.0, max);
        if (next - self.h_offset).abs() > f64::EPSILON {
            self.h_offset = next;
            self.layout_dirty = true;
            self.structure_dirty = true;
        }
    }

    /// The horizontal scroll offset of the columns.
    pub fn scroll_x(&self) -> f64 {
        self.h_offset
    }

    /// `None` when the bar is inactive (no viewport set, or rows fit).
    pub fn scrollbar_thumb(&self) -> Option<Rect> {
        let track = self.v_track()?;
        self.vbar.thumb_rect(track, self.v_metrics())
    }

    /// The bar beside the rows (`vertical`) or under the columns.
    fn draw_bar(&self, scene: &mut Scene, transform: Affine, theme: &Theme, vertical: bool) {
        let bars = if vertical {
            (self.v_track(), &self.vbar, self.v_metrics())
        } else {
            (self.h_track(), &self.hbar, self.h_metrics())
        };
        for (track, bar, metrics) in [bars] {
            let Some(track) = track else { continue };
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                crate::widget::scrollbar::track_color(theme),
                None,
                &RoundedRect::from_rect(track, track.width().min(track.height()) / 2.0),
            );
            bar.draw(scene, transform, track, metrics, theme, false);
        }
    }

    /// The first text column shown: a tree's chevron and indent, and the add
    /// row's placeholder, sit in it (never in a checkbox column).
    fn first_shown_column(&self) -> Option<usize> {
        (0..self.columns.len()).find(|&i| {
            !self.hidden.get(i).copied().unwrap_or(false)
                && self.columns.get(i).is_some_and(|c| c.checked.is_none())
        })
    }

    /// How far a tree row's content stands in: its depth in `s4` steps, then
    /// the chevron's slot.
    fn tree_indent(&self, theme: &Theme, depth: usize) -> f64 {
        let tokens = &theme.tokens;
        f64::from(tokens.spacing.s4) * depth as f64
            + f64::from(tokens.metrics.icon_sm)
            + f64::from(tokens.spacing.s1)
    }

    /// The chevron of tree row `index` in the first column, if it has one.
    fn tree_chevron(&self, index: usize, theme: &Theme) -> Option<Rect> {
        let tree = self.tree.as_ref()?;
        let Some(Arrival::Ready(row)) = self.row_state_at(index) else {
            return None;
        };
        if !(tree.has_children)(row) {
            return None;
        }
        let column = self.col_rects.get(self.first_shown_column()?)?;
        let row_rect = self.row_rect(index)?;
        let icon = f64::from(theme.tokens.metrics.icon_sm);
        let x =
            column.x0 + self.bleed + f64::from(theme.tokens.spacing.s4) * (tree.depth)(row) as f64;
        Some(Rect::new(
            x,
            row_rect.center().y - icon / 2.0,
            x + icon,
            row_rect.center().y + icon / 2.0,
        ))
    }

    /// Toggle tree row `index` to `expand`; `false` when it has no children
    /// or is already so.
    fn fold_tree_row(&mut self, index: usize, expand: bool) -> bool {
        let Some(tree) = &self.tree else {
            return false;
        };
        let state = match self.row_state_at(index) {
            Some(Arrival::Ready(row)) => Some(((tree.has_children)(row), (tree.expanded)(row))),
            _ => None,
        };
        let Some((has_children, expanded)) = state else {
            return false;
        };
        if !has_children || expanded == expand {
            return false;
        }
        if let Some(tree) = &mut self.tree {
            (tree.on_toggle)(index, expand);
        }
        true
    }

    /// The checkbox cell under `pt`: its row and column.
    fn check_cell_at(&self, pt: Point) -> Option<(usize, usize)> {
        let row = self.row_at(pt)?;
        let column = self
            .col_rects
            .iter()
            .position(|c| pt.x >= c.x0 && pt.x < c.x1)?;
        self.columns.get(column)?.checked.as_ref()?;
        Some((row, column))
    }

    fn toggle_check(&mut self, row: usize, column: usize) -> bool {
        let checked = match (self.row_state_at(row), self.columns.get(column)) {
            (Some(Arrival::Ready(data)), Some(col)) => {
                col.checked.as_ref().map(|checked| checked(data))
            }
            _ => None,
        };
        let Some(checked) = checked else {
            return false;
        };
        let source = self.source_row(row);
        if let Some(on_check) = &mut self.on_check {
            on_check(source, !checked);
        }
        true
    }

    /// The blank add row's rect, after the last row.
    fn add_row_rect(&self) -> Option<Rect> {
        self.add_row.as_ref()?;
        let band = self.columns_band();
        let y0 = self.rect.y0 + self.header_h + self.content_body_h() - self.viewport.offset();
        Some(Rect::new(band.x0, y0, band.x1, y0 + self.default_row_h))
    }

    fn drag_scroll_enabled(&self) -> bool {
        self.drag_scroll_enabled || !self.interaction.supports_hover()
    }

    fn open_row_context(&mut self, row: usize, point: Point) -> bool {
        if self.on_row_context.is_none() || row >= self.display_len() {
            return false;
        }
        if !self.is_row_selected(row) {
            self.select_only_index(row);
        }
        let rect = self.row_rect(row).unwrap_or(self.rect);
        let source = self.source_row(row);
        if let Some(callback) = &mut self.on_row_context {
            callback(source, point, rect);
        }
        true
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

    /// Sort by column `index`, or flip the order when it already sorts by
    /// it; the column's own comparator orders the rows (see
    /// [`ColumnDef::sorted_by`]). `None` if the column does not sort.
    pub fn toggle_sort(&mut self, index: usize) -> Option<(String, bool)> {
        let col = self.columns.get(index)?;
        if !col.sortable {
            return None;
        }
        let key = col.key.clone();
        // A sort the person clicked moves the shown rows to their new
        // places; rows coming from off-screen fade in at theirs. A sort a
        // key asked for lands.
        self.capture_reorder_motion();
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
        let sorted = if let Some(s) = &mut self.sort
            && s.key == key
        {
            s.ascending = ascending;
            self.reapply_sort();
            true
        } else {
            self.sort_by_column(&key, ascending)
        };
        if !sorted {
            self.moving.rows.cancel();
            return None;
        }
        if let Some(state) = &self.sort_state {
            state.set(Some(TableSort {
                key: key.clone(),
                ascending,
            }));
        }
        Some((key, ascending))
    }

    /// Header background, bottom border, column labels and the active sort arrow.
    fn draw_header(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.headerless {
            return;
        }
        let tokens = &theme.tokens;
        let band = self.columns_band();
        let header_rect = Rect::new(band.x0, self.rect.y0, band.x1, self.rect.y0 + self.header_h);
        // The header sits on the table's ground; one rule derived from that
        // ground closes it, on the band's last pixel row, as wide as the
        // rows' lift.
        crate::surface::separator(
            scene,
            transform,
            header_rect.x0,
            header_rect.x1,
            header_rect.y1 - 0.5,
            0.0,
            tokens.colors.surfaces.content,
        );
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &header_rect,
        );

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
                        .or(self.sort_indicator.as_ref())
                        .filter(|(key, _)| *key == col.key)
                        .map(|(_, ascending)| *ascending)
                });
            paint_header_label(
                &mut crate::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                Rect::new(cr.x0, header_rect.y0, cr.x1, header_rect.y1),
                &header,
                col.alignment(),
                sorted,
            );
        }
        scene.pop_layer();
        if self.ruled {
            self.draw_column_rules(scene, transform, theme);
        }
    }

    /// Vertical rules between columns, through the header and the body, from
    /// the ground they sit on.
    fn draw_column_rules(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let rule = crate::surface::edge_on(theme.tokens.colors.surfaces.content);
        let y1 = self.rect.y0 + self.header_h + self.body_viewport_h();
        let band = self.columns_band();
        for cr in self.col_rects.iter().skip(1) {
            if cr.width() <= 0.0 || cr.x0 <= band.x0 || cr.x0 >= band.x1 {
                continue;
            }
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
    /// content, windowed to [`Self::visible_row_range`] plus overscan: each
    /// row (and the add row) a part of the scrolled body, drawn where it
    /// stands in the rows as if unscrolled
    /// ([`crate::scroll_copy::Painted`]), so a row a scroll only moved
    /// encodes the same and a host copies it.
    fn draw_rows(
        &self,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) -> Vec<crate::scroll_copy::Piece<crate::scroll_copy::Painted>> {
        let tokens = &theme.tokens;
        let (vis_start, vis_end) = self.visible_row_range();
        let body_bottom = self.rect.y0 + self.header_h + self.body_viewport_h();
        let band = self.columns_band();
        let ground = tokens.colors.surfaces.content;
        let pressable = self.rows_pressable();
        let now = crate::motion::frame_now();
        let offset = self.viewport.offset();
        let mut parts = Vec::with_capacity(vis_end.saturating_sub(vis_start).saturating_add(1));
        for ri in vis_start..vis_end {
            let Some(row_state) = self.row_state_at(ri) else {
                continue;
            };
            // A row on its way after a sort or a change the person made is
            // drawn where it is on its way, an arriving one at its opacity
            // (`motion::ListChange`); the body clip trims what is outside.
            let (dy, alpha) = self
                .row_identity_at(ri)
                .map_or((0.0, 1.0), |key| self.moving.rows.pose(&key, now));
            // Internal virtualization subtracts the viewport offset; when
            // driven by an outer Scrollable, it stays zero and the whole rect is
            // translated instead — both paths render correctly.
            let Some(view_top) = self.viewport.item_view_top(ri) else {
                continue;
            };
            let Some(row_h) = self.viewport.item_height(ri) else {
                continue;
            };
            let y0 = self.rect.y0 + self.header_h + view_top + dy;
            let y1 = y0 + row_h;
            if y1 <= self.rect.y0 + self.header_h || y0 >= body_bottom {
                continue;
            }
            let shown = Rect::new(band.x0, y0, band.x1, y1);
            // Painted where the row stands in the rows, unscrolled.
            let (y0, y1) = (y0 + offset, y1 + offset);
            let mut row_scene = Scene::new();
            let scene = &mut row_scene;
            let faded = alpha < 1.0;
            if faded {
                scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    alpha,
                    transform,
                    &Rect::new(band.x0, y0, band.x1, y1),
                );
            }
            let selected = self.selectable && self.is_row_selected(ri);
            let hovered = pressable && self.hover_row == Some(ri);
            // Whole-row geometry: the caller's body clip layer trims the part
            // that scrolls past the viewport, so cells see their real row.
            // The lift and the stripe share one shape: the columns' band
            // (inset `s2`, clear of the scroll bar) at the item radius.
            let row_band = Rect::new(band.x0, y0, band.x1, y1);
            // Selection is a lift (HIG §9.3), never an accent wash, and
            // takes precedence over the zebra stripe; the stripe is a
            // fraction of the same lift, so selection always reads past it.
            if selected || hovered {
                // A pressable row lifts under the pointer, as a list row
                // and the painted table face do.
                crate::chrome::paint_row_selection(
                    scene,
                    transform,
                    row_band,
                    crate::chrome::SelectionState::new(selected, hovered, false),
                    ground,
                    theme,
                );
            } else if ri % 2 == 1 {
                let radius = f64::from(tokens.radii.item).min(row_band.height() / 2.0);
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    zebra_on(theme, ground),
                    None,
                    &RoundedRect::from_rect(row_band, radius),
                );
            }
            // A row on its way paints its placeholder in its slot; one that
            // landed fades in over it.
            let slot = Rect::new(band.x0 + self.bleed, y0, band.x1 - self.bleed, y1);
            let mut paint_row = |ctx: &mut PaintCtx<'_>| {
                let scene = &mut *ctx.scene;
                let text_renderer = &mut *ctx.text;
                match row_state {
                    Arrival::Ready(row) => {
                        let first = self.first_shown_column();
                        for (i, col) in self.columns.iter().enumerate() {
                            let mut cell_rect = match self.col_rects.get(i) {
                                Some(cr) if cr.width() > 0.0 => Rect::new(cr.x0, y0, cr.x1, y1),
                                _ => continue,
                            };
                            if first == Some(i)
                                && let Some(tree) = &self.tree
                            {
                                let depth = (tree.depth)(row);
                                if (tree.has_children)(row) {
                                    let icon = f64::from(tokens.metrics.icon_sm);
                                    let x = cell_rect.x0
                                        + self.bleed
                                        + f64::from(tokens.spacing.s4) * depth as f64;
                                    let glyph = Rect::new(
                                        x,
                                        cell_rect.center().y - icon / 2.0,
                                        x + icon,
                                        cell_rect.center().y + icon / 2.0,
                                    );
                                    let chevron = if (tree.expanded)(row) {
                                        crate::icons::Icon::ChevronDown
                                    } else {
                                        crate::icons::Icon::ChevronRight
                                    };
                                    chevron.draw(scene, transform, glyph, tokens.colors.text_dim);
                                }
                                cell_rect.x0 += self.tree_indent(theme, depth);
                            }
                            (col.render_cell)(
                                row,
                                scene,
                                transform,
                                theme,
                                text_renderer,
                                cell_rect,
                            );
                            if let Some(checked) = &col.checked {
                                let side = f64::from(tokens.metrics.icon_md);
                                crate::widget::Checkbox::paint(
                                    scene,
                                    transform,
                                    theme,
                                    &crate::widget::CheckboxFace::new(
                                        Rect::from_center_size(cell_rect.center(), (side, side)),
                                        checked(row),
                                    ),
                                );
                            }
                            if let Some(badge) = &col.badge {
                                let (label, tone) = badge(row);
                                paint_cell_badge(
                                    scene,
                                    transform,
                                    text_renderer,
                                    theme,
                                    cell_rect,
                                    col.alignment(),
                                    &label,
                                    tone,
                                );
                            }
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
                                tr(
                                    row,
                                    scene,
                                    transform,
                                    text_renderer,
                                    cell_rect,
                                    theme,
                                    CellState { selected, hovered },
                                );
                            }
                        }
                    }
                    // The placeholder of a row on its way is the arrivals'.
                    Arrival::Pending { .. } => {}
                    Arrival::Failed(message) => arrivals::paint_row_failure(
                        scene,
                        transform,
                        text_renderer,
                        theme,
                        &self.failure_label(ri, message),
                        slot,
                    ),
                }
            };
            let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);
            match self.model_key_at(ri) {
                Some(key) => self.arrivals.paint(
                    &key,
                    SkeletonShape::Text { lines: 1 },
                    &mut ctx,
                    slot,
                    now,
                    &mut paint_row,
                ),
                None => paint_row(&mut ctx),
            }
            if faded {
                scene.pop_layer();
            }
            let key = self.row_identity_at(ri).map_or_else(
                || format!("#{ri}"),
                |id| format!("{}:{}", id.value, id.occurrence),
            );
            parts.push(crate::scroll_copy::Piece {
                key: crate::scroll_copy::piece_key(key),
                rect: shown,
                paint: crate::scroll_copy::Painted(row_scene),
            });
        }
        if let (Some(add), Some(shown)) = (&self.add_row, self.add_row_rect())
            && shown.y0 < body_bottom
        {
            let rect = shown + vello::kurbo::Vec2::new(0.0, offset);
            let mut add_scene = Scene::new();
            let scene = &mut add_scene;
            // The blank row that adds one: its placeholder where the first
            // cell's text starts, in tertiary ink.
            let style = cell_style(theme, TextKind::Prose);
            let x = self
                .first_shown_column()
                .and_then(|i| self.col_rects.get(i))
                .map_or(rect.x0, |c| c.x0)
                + self.bleed;
            let icon = f64::from(tokens.metrics.icon_sm);
            let glyph = Rect::new(
                x,
                rect.center().y - icon / 2.0,
                x + icon,
                rect.center().y + icon / 2.0,
            );
            crate::icons::Icon::Plus.draw(scene, transform, glyph, tokens.colors.text_tertiary);
            let (_, h) = text_renderer.measure_style(&add.label, None, &style);
            text_renderer.draw_style(
                scene,
                &add.label,
                tokens.colors.text_tertiary,
                transform.pre_translate(
                    (
                        x + icon + f64::from(tokens.spacing.s1),
                        centered_text_top(rect.y0, rect.height(), h),
                    )
                        .into(),
                ),
                None,
                &style,
            );
            parts.push(crate::scroll_copy::Piece {
                key: crate::scroll_copy::piece_key("add"),
                rect: shown,
                paint: crate::scroll_copy::Painted(add_scene),
            });
        }
        parts
    }

    /// `WindowEvent::CursorMoved` — drag-resize/drag-scroll tracking, else
    /// scrollbar hover.
    fn on_cursor_moved(&mut self, pt: Point, handled: &mut bool) {
        self.last_cursor = Some(pt);
        if self.resize.is_some() {
            self.drag_resize(f32_of_f64(pt.x));
            *handled = true;
            return;
        }
        // An active scrollbar drag owns the move.
        if self.vbar.is_dragging() {
            if let Some(track) = self.v_track()
                && let Some(target) = self.vbar.drag_to(track, self.v_metrics(), pt)
            {
                self.set_scroll_y(f32_of_f64(target));
            }
            *handled = true;
            return;
        }
        if self.hbar.is_dragging() {
            if let Some(track) = self.h_track()
                && let Some(target) = self.hbar.drag_to(track, self.h_metrics(), pt)
            {
                self.set_h_offset(target);
            }
            *handled = true;
            return;
        }
        // A press on a row that moves past the slop begins a row drag.
        if let Some((row, origin)) = self.row_press
            && self.on_row_drag_begin.is_some()
            && (pt - origin).hypot() > DRAG_SLOP
        {
            self.row_press = None;
            self.deferred_click = None;
            let rect = self.row_rect(row).unwrap_or(self.rect);
            let source = self.source_row(row);
            if let Some(begin) = &mut self.on_row_drag_begin {
                begin(source, origin, rect);
            }
            *handled = true;
            return;
        }
        if self.drag_scroll.is_tracking() {
            let now = crate::time::now();
            if let Some(update) = self.drag_scroll.update(pt.y, now) {
                self.momentum.stop();
                self.set_scroll_offset(update.target_offset);
                *handled = true;
            }
            return;
        }
        if let Some(track) = self.v_track() {
            self.vbar.update_hover(track, self.v_metrics(), pt);
        } else {
            self.vbar.set_hover(false);
        }
        if let Some(track) = self.h_track() {
            self.hbar.update_hover(track, self.h_metrics(), pt);
        } else {
            self.hbar.set_hover(false);
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
                    self.set_scroll_y(f32_of_f64(target));
                    *handled = true;
                    return;
                }
                PressOutcome::Miss => {}
            }
        }
        if let Some(track) = self.h_track() {
            match self.hbar.press(track, self.h_metrics(), pt) {
                PressOutcome::Thumb => {
                    *handled = true;
                    return;
                }
                PressOutcome::Page(target) => {
                    self.set_h_offset(target);
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
                self.begin_resize(&key, f32_of_f64(pt.x));
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
        // 3. The blank add row, a tree row's chevron and a checkbox cell
        // act before the row is selected.
        if let Some(rect) = self.add_row_rect()
            && rect.contains(pt)
            && pt.y < self.body_rect().y1
        {
            if let Some(add) = &mut self.add_row {
                (add.on_add)();
            }
            *focus_target = Some(node_id);
            *handled = true;
            return;
        }
        if let Some(theme) = self.measure_theme.clone()
            && let Some(row) = self.row_at(pt)
            && self
                .tree_chevron(row, &theme)
                .is_some_and(|chevron| chevron.inflate(4.0, 4.0).contains(pt))
        {
            let expanded = match (self.tree.as_ref(), self.row_state_at(row)) {
                (Some(tree), Some(Arrival::Ready(data))) => (tree.expanded)(data),
                _ => false,
            };
            self.fold_tree_row(row, !expanded);
            *focus_target = Some(node_id);
            *handled = true;
            return;
        }
        if let Some((row, column)) = self.check_cell_at(pt) {
            self.toggle_check(row, column);
            *focus_target = Some(node_id);
            *handled = true;
            return;
        }
        // 4. A body that drags to scroll (touch, or opted in) defers row
        // activation until release so the gesture can become a drag.
        let in_body = pt.x >= self.rect.x0
            && pt.x <= self.rect.x1
            && pt.y >= self.rect.y0 + self.header_h
            && pt.y <= self.rect.y1;
        if self.internal_scroll() && in_body && self.drag_scroll_enabled() {
            self.drag_scroll
                .begin(pt.y, self.viewport.offset(), crate::time::now());
            *focus_target = Some(node_id);
            *handled = true;
            return;
        }
        if let Some(row) = self.row_at(pt) {
            self.row_press = Some((row, pt));
        }
        self.select_row_at_cursor(modifiers, handled, focus_target, node_id);
        if in_body && self.row_at(pt).is_some() {
            *focus_target = Some(node_id);
        }
    }

    fn select_row_at_cursor(
        &mut self,
        modifiers: winit::event::Modifiers,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
    ) {
        let Some(pt) = self.last_cursor else { return };
        if let Some(ri) = self.row_at(pt)
            && self.retry_failed_row(ri)
        {
            *handled = true;
            return;
        }
        if self.selectable
            && let Some(ri) = self.row_at(pt)
        {
            let ctrl = modifiers.state().control_key() || modifiers.state().super_key();
            let shift = modifiers.state().shift_key();
            // Dragging several selected rows starts with a plain press on
            // one of them: it must not select that row alone first.
            if !ctrl
                && !shift
                && self.on_row_drag_begin.is_some()
                && self.selection.selected().len() > 1
                && self.selection.selected().contains(&ri)
            {
                self.deferred_click = Some(ri);
                *focus_target = Some(node_id);
                *handled = true;
                return;
            }
            self.click_row(ri, ctrl, shift, focus_target, node_id);
            *handled = true;
        }
    }

    /// A click on display row `ri`: the selection, the host's click, and a
    /// double click's activation.
    fn click_row(
        &mut self,
        ri: usize,
        ctrl: bool,
        shift: bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
    ) {
        self.apply_selection_click(ri, ctrl, shift);
        *focus_target = Some(node_id);
        let source = self.source_row(ri);
        if let Some(cb) = &mut self.on_row_click {
            cb(source, ctrl, shift);
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
                cb(source);
            }
        } else {
            self.last_click_row = Some((ri, now));
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
        // A deferred press that became no drag is the click it was.
        if let Some(ri) = self.deferred_click.take()
            && self.row_press.is_some()
        {
            self.click_row(ri, false, false, focus_target, node_id);
            did = true;
        }
        self.row_press = None;
        if self.resize.is_some() {
            self.end_resize();
            did = true;
        }
        if self.vbar.release() {
            did = true;
        }
        if self.hbar.release() {
            did = true;
        }
        if let Some(drag) = self.drag_scroll.finish(crate::time::now()) {
            if drag.dragged {
                if self.reduce_motion {
                    self.momentum.stop();
                } else {
                    self.momentum.fling(drag.velocity);
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
    fn on_wheel(
        &mut self,
        delta: &MouseScrollDelta,
        phase: TouchPhase,
        sideways: bool,
        handled: &mut bool,
    ) {
        // Shift turns the wheel sideways, as does a trackpad's horizontal
        // swipe, over columns wider than the table.
        if self.h_overflow() {
            let (dx, dy) = match delta {
                MouseScrollDelta::LineDelta(x, y) => (
                    f64::from(*x) * self.default_row_h,
                    f64::from(*y) * self.default_row_h,
                ),
                MouseScrollDelta::PixelDelta(p) => (p.x, p.y),
            };
            let along = if sideways {
                -(if dx.abs() > dy.abs() { dx } else { dy })
            } else if dx.abs() > dy.abs() {
                -dx
            } else {
                0.0
            };
            if along != 0.0 {
                let before = self.h_offset;
                self.set_h_offset(before + along);
                *handled = (self.h_offset - before).abs() > f64::EPSILON;
                return;
            }
        }
        if self.internal_scroll() {
            self.drag_scroll.cancel();
            *handled = match self.momentum.wheel(delta, phase, self.reduce_motion) {
                WheelStep::Scroll(dy) => {
                    let before = self.viewport.offset();
                    self.set_scroll_offset(before + dy);
                    self.momentum
                        .wheel_applied(dy, self.viewport.offset() - before)
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
                let source = cur.map(|c| self.source_row(c));
                if let Some(c) = source
                    && let Some(cb) = &mut self.on_activate
                {
                    cb(c);
                }
                *handled = true;
            }
            Key::Named(NamedKey::ArrowRight) if self.tree.is_some() => {
                *handled = cur.is_some_and(|c| self.fold_tree_row(c, true));
            }
            Key::Named(NamedKey::ArrowLeft) if self.tree.is_some() => {
                *handled = cur.is_some_and(|c| self.fold_tree_row(c, false));
            }
            Key::Named(NamedKey::Space) => {
                let column = self.columns.iter().position(|c| c.checked.is_some());
                if let (Some(c), Some(column)) = (cur, column) {
                    *handled = self.toggle_check(c, column);
                }
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
}

#[path = "data_table_arrivals.rs"]
mod arrivals;

const HEADER_HEIGHT: f64 = 32.0;
/// How far a press on a row travels before it becomes a row drag.
const DRAG_SLOP: f64 = 4.0;
/// A regular row at the stock theme: a pointer target and a step (40); a
/// compact row is a pointer target (32). The live values come from the
/// theme's metrics.
const ROW_HEIGHT: f64 = 40.0;

/// A column header: `Caption` in its strong weight, sentence case.
fn header_style(theme: &Theme) -> crate::text_render::TextStyle {
    theme.text(TextRole::Caption).strong()
}

/// A cell's text style: `Caption`, set for the column's [`TextKind`].
fn cell_style(theme: &Theme, kind: TextKind) -> crate::text_render::TextStyle {
    theme.text(TextRole::Caption).with_kind(kind)
}

/// Paint one text cell placed by the column's alignment, elided to the cell,
/// centred on the row; an absent value is the column's label in tertiary ink.
fn paint_cell_text<Row: ?Sized>(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    cell: Rect,
    column: &ColumnDef<Row>,
    value: Option<&str>,
) {
    let style = cell_style(theme, column.kind);
    let align = column.alignment();
    let pad = f64::from(theme.tokens.spacing.s2);
    let (label, ink) = match value {
        Some(value) => (value, theme.text_primary),
        None => (column.absent.as_str(), theme.text_tertiary),
    };
    let room = f32_of_f64((cell.width() - 2.0 * pad).max(0.0));
    if column.max_lines > 1 {
        // A wrapping cell: its lines centred on the row as one block.
        let lines = text.clamp_to_lines(label, &style, room, column.max_lines);
        let line_h = f64::from(style.line_height_px());
        let block = line_h * lines.len() as f64;
        let mut y = centered_text_top(cell.y0, cell.height(), f32_of_f64(block));
        for line in &lines {
            let (w, _) = text.measure_style(line, None, &style);
            let x = align.place(cell, f64::from(w), pad);
            text.draw_style(
                scene,
                line,
                ink,
                transform.pre_translate((x, y).into()),
                None,
                &style,
            );
            y += line_h;
        }
        return;
    }
    let label = text.elide_to_width_style(label, room, &style);
    let (w, h) = text.measure_style(&label, None, &style);
    let x = align.place(cell, f64::from(w), pad);
    text.draw_style(
        scene,
        &label,
        ink,
        transform.pre_translate((x, centered_text_top(cell.y0, cell.height(), h)).into()),
        None,
        &style,
    );
}

/// Paint a cell's badge: a quiet pill tinted with its tone, placed across
/// the cell by the column's alignment and centred on the row.
fn paint_cell_badge(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    cell: Rect,
    align: CellAlign,
    label: &str,
    tone: crate::widget::Tone,
) {
    if label.is_empty() {
        return;
    }
    let pad = f64::from(theme.tokens.spacing.s2);
    let w = crate::widget::BadgeFace::intrinsic_width(label, text, theme)
        .min((cell.width() - 2.0 * pad).max(0.0));
    let h = crate::widget::BadgeFace::height(theme);
    let x = align.place(cell, w, pad);
    let style = if tone == crate::widget::Tone::Neutral {
        crate::widget::ChipStyle::Neutral
    } else {
        crate::widget::ChipStyle::Tinted(tone)
    };
    crate::widget::BadgeFace::new(
        Rect::new(
            x,
            cell.center().y - h / 2.0,
            x + w,
            cell.center().y + h / 2.0,
        ),
        label,
    )
    .style(style)
    .paint(scene, transform, text, theme);
}

/// Row-stacking variant of [`crate::text_metrics::centered_text_top`]: text
/// taller than the band starts at the band's top rather than bleeding into
/// the row above, which the unclamped rule would let it do.
pub(crate) fn centered_text_top(band_y0: f64, band_h: f64, text_h: f32) -> f64 {
    crate::text_metrics::centered_text_top(band_y0, band_h, f64::from(text_h)).max(band_y0)
}

fn accessible_sort_description<Row: ?Sized>(
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

impl Widget for DataTableCore {
    /// Rows fading in after a long reveal, on their way after a sort or a
    /// change, or whose placeholder steps on, repaint the table; otherwise
    /// what its mounted cells say.
    fn needs_redraw(&self) -> bool {
        self.fade_drawn < 1.0
            || self.fade_shown < 1.0
            || self.momentum.give_needs_frame(crate::motion::frame_now())
            || self.moving.rows.is_moving(crate::motion::frame_now())
            || self.arrivals.needs_redraw(crate::motion::frame_now())
            || self
                .mounted_cells
                .iter()
                .any(|cell| cell.widget.needs_redraw())
    }

    /// What the draw changed: the rows that changed, and a scroll offered as
    /// a copy of the rows that only moved
    /// ([`crate::scroll_copy::PaintedFrames`]);
    /// the cells say what changed in them.
    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        self.collect_damage_of(self, regions);
    }

    fn mark_drawn(&mut self) {
        self.fade_shown = self.fade_drawn;
        self.shown.present();
        for cell in &mut self.mounted_cells {
            cell.widget.mark_drawn();
        }
    }

    fn release_scene_cache(&mut self) {
        self.shown.forget();
        for cell in &mut self.mounted_cells {
            cell.widget.release_scene_cache();
        }
    }

    /// Mounting the cells a scroll or the pointer brought near is
    /// described: they come and go inside the rows the table repaints or a
    /// host copies, and a mounted cell draws itself whole. Any other change
    /// is not.
    fn take_structure_change(&mut self) -> crate::widget::StructureChange {
        use crate::widget::StructureChange;
        let mut change = if std::mem::take(&mut self.structure_dirty) {
            std::mem::take(&mut self.remount);
            StructureChange::Rebuild
        } else if std::mem::take(&mut self.remount) {
            StructureChange::Described
        } else {
            StructureChange::None
        };
        for cell in &mut self.mounted_cells {
            change = change.join(cell.widget.take_structure_change());
        }
        change
    }

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

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let model_changes = self
            .model
            .as_deref_mut()
            .map_or_else(Vec::new, TableModel::poll_changes);
        let received_changes = !model_changes.is_empty();
        if received_changes {
            self.notify_model_changes(&model_changes);
        }
        let now = crate::time::now();
        self.observe_arrivals(crate::motion::frame_now());
        let mut children_changed = false;
        for cell in &mut self.mounted_cells {
            children_changed |= cell.widget.update(state, path);
        }
        // A reveal in progress, like a glide, moves only the viewport.
        self.follow_scroll_to(crate::motion::frame_now());
        if !self.momentum.is_active() {
            self.refresh_view_anchor();
            return received_changes || children_changed;
        }
        // A glide moves only the viewport, which the frame it asks for lays
        // out; rows it brings near mount through that layout's rebuild. It
        // is no change of the model, so it does not report one (as
        // `Scrollable::update`).
        let delta = self.momentum.advance(now);
        let before = self.viewport.offset();
        self.set_scroll_offset(before + delta);
        if self.viewport.offset().to_bits() == before.to_bits() {
            self.momentum.stop();
        }
        self.refresh_view_anchor();
        received_changes || children_changed
    }

    fn composition_follows_allocation(&self) -> bool {
        true
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
        self.take_up_row_motion();
        self.apply_type_metrics(theme);
        self.measure_theme = Some(theme.clone());
        self.motion = theme.motion();
        self.reconcile_mounted_cells();
        let mut child_nodes = Vec::with_capacity(self.mounted_cells.len());
        for cell in &mut self.mounted_cells {
            let child = cell.widget.build_layout(taffy, theme, node_map);
            let mut style = taffy.style(child).cloned().unwrap_or_default();
            style.position = taffy::Position::Absolute;
            // A leading column's control fills it; a centred or trailing one
            // keeps its own width, at most the column's, so it can stand
            // where the column's alignment puts it.
            let fills = self
                .columns
                .get(cell.column_index)
                .is_none_or(|column| column.alignment() == CellAlign::Leading);
            if let Some(column) = self.col_rects.get(cell.column_index) {
                style.inset.left = length(f32_of_f64(column.x0 - self.rect.x0));
                if fills {
                    style.size.width = Dimension::length(f32_of_f64(column.width()));
                } else {
                    style.max_size.width = Dimension::length(f32_of_f64(column.width()));
                }
            } else if let Some(column) = self.columns.get(cell.column_index)
                && let ColumnWidth::Fixed(width) = column.width
            {
                if fills {
                    style.size.width = Dimension::length(width);
                } else {
                    style.max_size.width = Dimension::length(width);
                }
            }
            let top = self
                .viewport
                .item_view_top(cell.row_index)
                .unwrap_or_default();
            style.inset.top = length(f32_of_f64(self.header_h + top));
            let _ = taffy.set_style(child, style);
            cell.node = Some(child);
            child_nodes.push(child);
        }
        let add_row = if self.add_row.is_some() {
            self.default_row_h
        } else {
            0.0
        };
        let body_height = if self.viewport.viewport() > 0.0 {
            self.viewport.viewport()
        } else {
            self.viewport
                .content_extent()
                .max(self.viewport.estimated_height())
                + add_row
        };
        let height = self.header_h + body_height + self.h_reserve;
        let style = if self.fill_height {
            // The parent's height: the rows scroll inside it.
            Style {
                size: Size {
                    width: percent(1.0_f32),
                    height: Dimension::auto(),
                },
                min_size: Size {
                    width: Dimension::auto(),
                    height: Dimension::length(f32_of_f64(self.header_h + self.default_row_h)),
                },
                flex_grow: 1.0,
                flex_shrink: 1.0,
                flex_basis: Dimension::length(0.0),
                overflow: taffy::Point {
                    x: taffy::Overflow::Clip,
                    y: taffy::Overflow::Clip,
                },
                ..Default::default()
            }
        } else {
            Style {
                size: Size {
                    width: percent(1.0_f32),
                    height: Dimension::length(f32_of_f64(height)),
                },
                overflow: taffy::Point {
                    x: taffy::Overflow::Clip,
                    y: taffy::Overflow::Clip,
                },
                ..Default::default()
            }
        };
        let node = taffy.parent_node(style, &child_nodes);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        self.structure_dirty = false;
        self.remount = false;
        node
    }

    /// Measure [`ColumnWidth::Auto`] columns from their header and the
    /// visible cells, never narrower than four characters' worth of line.
    fn adjust_layout_style(
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
        let first = self.first_shown_column();
        let mut next = HashMap::new();
        for (column_index, col) in self.columns.iter().enumerate() {
            if col.width != ColumnWidth::Auto {
                continue;
            }
            let label = crate::i18n::I18nManager::lookup_plain(&col.header);
            let mut width = f64::from(text.measure_style(&label, None, &header).0)
                + if col.sortable { sort_room } else { 0.0 };
            if col.checked.is_some() {
                width = width.max(f64::from(tokens.metrics.icon_md));
            }
            let style = cell_style(&theme, col.kind);
            for ri in start..end {
                let Some(Arrival::Ready(row)) = self.row_state_at(ri) else {
                    continue;
                };
                let indent = match &self.tree {
                    Some(tree) if first == Some(column_index) => {
                        self.tree_indent(&theme, (tree.depth)(row))
                    }
                    _ => 0.0,
                };
                if let Some(value) = &col.text {
                    let cell = value(row).unwrap_or_else(|| col.absent.clone());
                    width =
                        width.max(f64::from(text.measure_style(&cell, None, &style).0) + indent);
                }
                if let Some(badge) = &col.badge {
                    let (label, _) = badge(row);
                    if !label.is_empty() {
                        width = width.max(
                            crate::widget::BadgeFace::intrinsic_width(&label, text, &theme)
                                + indent,
                        );
                    }
                }
            }
            // Widget cells: the widest mounted control's own measure.
            for cell in &mut self.mounted_cells {
                if cell.column_index == column_index {
                    let measured = cell.widget.measure(
                        taffy::Size {
                            width: None,
                            height: None,
                        },
                        taffy::Size {
                            width: taffy::AvailableSpace::MaxContent,
                            height: taffy::AvailableSpace::MaxContent,
                        },
                        text,
                    );
                    width = width.max(f64::from(measured.width) - pad);
                }
            }
            next.insert(col.key.clone(), f32_of_f64((width + pad).max(floor).ceil()));
        }
        // Wrapping cells grow their rows to hold their lines.
        let mut grew = false;
        if self.columns.iter().any(|c| c.max_lines > 1) {
            let line = f64::from(theme.text(TextRole::Caption).line_height_px());
            let pad_y = f64::from(tokens.spacing.s2);
            for ri in start..end {
                let Some(Arrival::Ready(row)) = self.row_state_at(ri) else {
                    continue;
                };
                let mut lines = 1_usize;
                for (i, col) in self.columns.iter().enumerate() {
                    if col.max_lines <= 1 {
                        continue;
                    }
                    let (Some(value), Some(cr)) = (&col.text, self.col_rects.get(i)) else {
                        continue;
                    };
                    if cr.width() <= 0.0 {
                        continue;
                    }
                    let cell = value(row).unwrap_or_default();
                    let room = f32_of_f64((cr.width() - pad).max(1.0));
                    let n = text
                        .clamp_to_lines(&cell, &cell_style(&theme, col.kind), room, col.max_lines)
                        .len();
                    lines = lines.max(n);
                }
                let height =
                    (line * lines as f64 + 2.0 * pad_y).max(self.viewport.estimated_height());
                if let Some(key) = self.row_identity_at(ri) {
                    grew |= self.viewport.measure(ri, key, height);
                }
            }
        }
        if next == self.auto_widths && !grew {
            return false;
        }
        self.auto_widths = next;
        true
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        if self.fill_height {
            // The rows scroll inside whatever height the parent gave.
            let body = (self.rect.height() - self.header_h - self.h_reserve).max(0.0);
            if (body - self.viewport.viewport()).abs() > 0.5 {
                self.viewport.set_viewport(body);
                self.viewport
                    .set_overscan(self.viewport.estimated_height() * 2.0);
                self.structure_dirty = true;
                self.layout_dirty = true;
            }
        }
        // User-resized widths in `col_widths_px` override the declared
        // `ColumnWidth`; columns give way by priority when the band is
        // narrow, and what still does not fit scrolls sideways.
        let band = self.columns_band();
        self.hidden = hidden_for_room(
            &self.columns,
            band.width(),
            &self.col_widths_px,
            &self.auto_widths,
        );
        let (mut next_columns, content_w) = compute_column_rects(
            &self.columns,
            band,
            &self.col_widths_px,
            &self.auto_widths,
            &self.hidden,
        );
        self.content_w = content_w;
        // Keep a gutter under the rows for the sideways bar while the columns
        // overflow; the next layout makes the room.
        let reserve = if content_w > band.width() + 0.5 {
            self.gutter
        } else {
            0.0
        };
        if (reserve - self.h_reserve).abs() > f64::EPSILON {
            self.h_reserve = reserve;
            self.structure_dirty = true;
            self.layout_dirty = true;
        }
        let max_h = (content_w - band.width()).max(0.0);
        self.h_offset = self.h_offset.clamp(0.0, max_h);
        for rect in &mut next_columns {
            *rect = *rect - vello::kurbo::Vec2::new(self.h_offset, 0.0);
        }
        let columns_changed = self.col_rects != next_columns;
        self.col_rects = next_columns;
        let mut row_heights = HashMap::<usize, f64>::new();
        for cell in &mut self.mounted_cells {
            let Some(child) = cell.node else { continue };
            let child_layout = taffy.layout(child).copied().unwrap_or_default();
            // A control stands where its column's alignment puts it, flush
            // with the column's edge as a leading or trailing one.
            let width = f64::from(child_layout.size.width);
            let desired_x = self
                .col_rects
                .get(cell.column_index)
                .map_or(self.rect.x0, |column| {
                    self.columns
                        .get(cell.column_index)
                        .map_or(CellAlign::Leading, ColumnDef::alignment)
                        .place(*column, width, 0.0)
                });
            // A control centres on its row's line.
            let row_top = self.rect.y0
                + self.header_h
                + self
                    .viewport
                    .item_view_top(cell.row_index)
                    .unwrap_or_default();
            let row_h = self
                .viewport
                .item_height(cell.row_index)
                .unwrap_or_default();
            let desired_y =
                row_top + ((row_h - f64::from(child_layout.size.height)) / 2.0).max(0.0);
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
        self.corners = crate::widget::scrollbar::SurfaceCorners::seen(theme, transform);
        self.reduce_motion = theme.reduce_motion;
        self.motion = theme.motion();
        self.momentum.set_view(
            self.viewport.viewport(),
            self.internal_scroll(),
            self.motion,
        );
        if self.reduce_motion {
            self.momentum.stop();
        }
        let now = crate::motion::frame_now();
        self.observe_arrivals(now);
        self.arrivals.mark_drawn(now);
        // After a long reveal the rows fade in where they landed.
        self.fade_drawn = self.scroll_to.opacity(now);
        // What does not scroll with the rows: the header, the focus ring and
        // the sideways bar; compared frame to frame as one.
        let mut outside = Scene::new();
        // No ground of its own: the table stands on its region's ground.
        self.draw_header(&mut outside, transform, text_renderer, theme);
        scene.append(&outside, None);
        // The body clips at the band, so the lift and stripe bleed past the
        // table's content edges as a list row's do.
        let band = self.columns_band();
        let body = Rect::new(
            band.x0,
            self.rect.y0 + self.header_h,
            band.x1,
            self.rect.y0 + self.header_h + self.body_viewport_h(),
        );
        // Rows and mounted cells paint whole-row geometry inside one body
        // clip, so a row leaving the viewport is trimmed rather than re-laid.
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            self.fade_drawn,
            transform,
            &body,
        );
        // Each row is drawn unscrolled and lands scrolled.
        let pieces = self.draw_moving_rows(scene, transform, text_renderer, theme, presence);
        scene.pop_layer();

        let mut chrome = Scene::new();
        // Focus ring, so arrow-key navigation reads as a focused control.
        if self.is_focused {
            crate::widget::paint_focus_ring(
                &mut chrome,
                transform,
                self.rect.inset(-1.0),
                0.0,
                theme,
            );
        }
        self.draw_bar(&mut chrome, transform, theme, false);
        scene.append(&chrome, None);
        outside.append(&chrome, None);
        // The bar over the rows stands still while they scroll under it.
        let mut fixed = Vec::new();
        if let Some(track) = self.v_track() {
            let mut bar = Scene::new();
            self.draw_bar(&mut bar, transform, theme, true);
            scene.append(&bar, None);
            fixed.push(crate::scroll_copy::Piece {
                key: crate::scroll_copy::piece_key("bar"),
                rect: track,
                paint: crate::scroll_copy::Painted(bar),
            });
        }
        let offset = self.viewport.offset() + self.give();
        if let Some(frame) = crate::scroll_copy::Showing::painted(
            vello::kurbo::Vec2::new(0.0, offset),
            body,
            crate::scroll_copy::on_screen(transform),
            crate::scroll_copy::Around {
                bounds: self.paint_bounds(),
                outside: crate::scroll_copy::Painted(outside),
            },
            pieces,
            fixed,
        ) {
            self.shown.note(frame);
        }
        self.refresh_view_anchor();
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        self.momentum.set_device_scale(ctx.scale_factor);
        self.device_scale = ctx.scale_factor;
        // The hand takes over a reveal in progress from where it is drawn.
        if matches!(
            event,
            WindowEvent::MouseWheel { .. }
                | WindowEvent::KeyboardInput { .. }
                | WindowEvent::Touch(_)
                | WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    ..
                }
        ) {
            self.scroll_to.take_over(crate::motion::frame_now());
        }
        // The row under the pointer, before a control in it takes the move.
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let pt = ctx.logical(position);
                self.last_cursor = Some(pt);
                self.track_hover(Some(pt));
            }
            WindowEvent::CursorLeft { .. } => self.track_hover(None),
            _ => {}
        }
        if self.resize.is_none() && !self.vbar.is_dragging() && !self.drag_scroll.is_tracking() {
            crate::widget::route_to_mounted(
                &mut self.mounted_cells,
                |mounted| (&mut mounted.widget, mounted.node),
                event,
                ctx,
            );
        }
        if *ctx.handled {
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let pt = ctx.logical(position);
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
            WindowEvent::MouseWheel { delta, phase, .. }
                if !crate::content_zoom::is_zoom_scroll(event, ctx.modifiers) =>
            {
                let sideways = ctx.modifiers.state().shift_key();
                let before = *ctx.handled;
                self.on_wheel(delta, *phase, sideways, ctx.handled);
                // It moved only this table's view: nothing the app publishes.
                if *ctx.handled && !before {
                    crate::widget::note_view_only_pan();
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
                if let Some(pt) = self.last_cursor
                    && let Some(row) = self.row_at(pt)
                    && self.open_row_context(row, pt)
                {
                    *ctx.focus_target = Some(ctx.node_id);
                    *ctx.handled = true;
                }
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
            || self.momentum.give_needs_frame(crate::motion::frame_now())
            || self.moving.rows.is_moving(crate::motion::frame_now())
            || self.reveal_in_progress()
            || self.arrivals.is_moving(crate::motion::frame_now())
            || self.model.as_deref().is_some_and(TableModel::needs_poll)
            || self
                .mounted_cells
                .iter()
                .any(|cell| cell.widget.needs_animation_frame())
    }

    fn next_wake_deadline(&self) -> Option<crate::time::Instant> {
        self.arrivals
            .next_wake_deadline(crate::time::now())
            .into_iter()
            .chain(super::defaults::next_wake_deadline(&|visit| {
                self.for_each_child(visit);
            }))
            .min()
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
        let mut dirty =
            std::mem::take(&mut self.structure_dirty) | std::mem::take(&mut self.remount);
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
        let mut node = AccessNode::new(Role::Table).with_state(
            AccessState::new()
                .focusable(self.is_focusable())
                .focused(self.is_focused()),
        );
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
                    .filter(|(ci, _)| !self.hidden.get(*ci).copied().unwrap_or(false))
                    .map(|(ci, col)| {
                        let role = if col.checked.is_some() {
                            Role::Checkbox
                        } else {
                            Role::Cell
                        };
                        let mut cell = AccessNode::new(role)
                            .with_name(crate::i18n::I18nManager::lookup_plain(&col.header))
                            .with_position(ci.saturating_add(1), col_count)
                            .with_key(ci as u64);
                        if col.checked.is_some()
                            && let Some(identity) = self.row_identity_at(ri)
                        {
                            cell = cell.with_action_target(format!(
                                "cell:{}:{}:{}",
                                identity.value, identity.occurrence, col.key
                            ));
                        }
                        if let Some(Arrival::Ready(row)) = row_state
                            && let Some(checked) = &col.checked
                        {
                            cell = cell.with_state(AccessState::new().checked(checked(row)));
                        }
                        if let Some(Arrival::Ready(row)) = row_state
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
                        busy: matches!(row_state, Some(Arrival::Pending { .. })),
                        invalid: matches!(row_state, Some(Arrival::Failed(_))),
                        ..Default::default()
                    })
                    // Only a viewport slice is exported, so its ordinal is not
                    // the absolute row. Key it by display position for stable
                    // AccessKit routing across scroll windows.
                    .with_key(ri as u64)
                    .with_children(cells);
                row = match row_state {
                    Some(Arrival::Pending { .. }) => {
                        row.with_name(crate::i18n::I18nManager::lookup_plain("arriving-loading"))
                    }
                    Some(Arrival::Failed(message)) => row.with_name(message),
                    // A ready row is named by its first column that has a
                    // value (its row header), as a person reads the table.
                    Some(Arrival::Ready(data)) => {
                        match self
                            .columns
                            .iter()
                            .enumerate()
                            .filter(|(ci, _)| !self.hidden.get(*ci).copied().unwrap_or(false))
                            .find_map(|(_, col)| col.accessible_value.as_ref())
                            .map(|value| value(data))
                            .filter(|name| !name.is_empty())
                        {
                            Some(name) => row.with_name(name),
                            None => row,
                        }
                    }
                    None => row,
                };
                if let Some(bounds) = row_bounds {
                    row = row.with_bounds(bounds);
                }
                if let Some(tree) = &self.tree
                    && let Some(Arrival::Ready(data)) = row_state
                {
                    row = row.with_level((tree.depth)(data).saturating_add(1));
                    if (tree.has_children)(data) {
                        row.state.expanded = Some((tree.expanded)(data));
                    }
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
    /// checkbox cells toggle their own checked value; other cells activate
    /// their row.
    ///
    /// Rows carry no explicit key, so the path element is the row's display
    /// index (its ordinal among the table's synthetic children); the
    /// callbacks it reaches name the host's row, as `on_row_click` does.
    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        key_path: &[u64],
        _state: &mut crate::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        use crate::a11y::Action;
        let Some(&row_key) = key_path.first() else {
            return false;
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
                if let Some(&column_key) = key_path.get(1) {
                    let Ok(column) = usize::try_from(column_key) else {
                        return false;
                    };
                    let Some(cell) = self.columns.get(column) else {
                        return false;
                    };
                    if cell.checked.is_some() {
                        return self.toggle_check(row, column);
                    }
                    if matches!(action, Action::ToggleCheck) {
                        return false;
                    }
                }
                self.select_only_index(row);
                let source = self.source_row(row);
                if let Some(cb) = &mut self.on_activate {
                    cb(source);
                }
                self.refresh_view_anchor();
                true
            }
            Action::ScrollIntoView => {
                self.viewport.scroll_to_index(row, ScrollAlignment::Nearest);
                self.refresh_view_anchor();
                true
            }
            Action::Expand => self.fold_tree_row(row, true),
            Action::Collapse => self.fold_tree_row(row, false),
            Action::ShowContextMenu => {
                let point = self
                    .row_rect(row)
                    .map_or(self.rect.center(), |r| r.center());
                self.open_row_context(row, point)
            }
            _ => false,
        }
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        _state: &mut crate::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        use crate::a11y::Action;

        if let Some(cell) = target.strip_prefix("cell:") {
            let mut parts = cell.splitn(3, ':');
            let (Some(value), Some(occurrence), Some(column)) =
                (parts.next(), parts.next(), parts.next())
            else {
                return false;
            };
            let Some(row) = self.semantic_row_index(&format!("row:{value}:{occurrence}")) else {
                return false;
            };
            let Some(column) = self
                .columns
                .iter()
                .position(|candidate| candidate.key == column)
            else {
                return false;
            };
            return if matches!(action, Action::Activate | Action::ToggleCheck) {
                self.selectable && self.toggle_check(row, column)
            } else {
                self.perform_semantic_action(
                    &format!("row:{value}:{occurrence}"),
                    action,
                    _state,
                    _path,
                )
            };
        }
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
                let source = self.source_row(index);
                if let Some(callback) = &mut self.on_activate {
                    callback(source);
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

/// The table's widget is its core's: a `DataTable<Row>` names its rows'
/// type, and nothing else.
impl<Row: 'static> Widget for DataTable<Row> {
    fn set_interaction_environment(
        &mut self,
        environment: crate::interaction::InteractionEnvironment,
    ) {
        Widget::set_interaction_environment(&mut self.core, environment);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        Widget::update(&mut self.core, state, path)
    }

    fn composition_follows_allocation(&self) -> bool {
        Widget::composition_follows_allocation(&self.core)
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        Widget::build_layout(&mut self.core, taffy, theme, node_map)
    }

    fn adjust_layout_style(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        text: &mut TextRenderer,
    ) -> bool {
        Widget::adjust_layout_style(&mut self.core, taffy, node, text)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        Widget::apply_layout(&mut self.core, taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        Widget::draw(
            &mut self.core,
            scene,
            transform,
            text_renderer,
            theme,
            presence,
        );
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        Widget::handle_event(&mut self.core, event, ctx);
    }

    fn wants_pointer_capture(&self) -> bool {
        Widget::wants_pointer_capture(&self.core)
    }

    fn needs_animation_frame(&self) -> bool {
        Widget::needs_animation_frame(&self.core)
    }

    fn needs_redraw(&self) -> bool {
        Widget::needs_redraw(&self.core)
    }

    fn children(&self) -> Vec<&dyn Widget> {
        Widget::children(&self.core)
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        Widget::for_each_child(&self.core, visitor);
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        Widget::children_mut(&mut self.core)
    }

    fn accessibility_child_path(&self, child_index: usize) -> Option<Vec<usize>> {
        Widget::accessibility_child_path(&self.core, child_index)
    }

    fn take_structure_dirty(&mut self) -> bool {
        Widget::take_structure_dirty(&mut self.core)
    }

    fn take_structure_change(&mut self) -> crate::widget::StructureChange {
        Widget::take_structure_change(&mut self.core)
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        self.core.collect_damage_of(self, regions);
    }

    fn mark_drawn(&mut self) {
        Widget::mark_drawn(&mut self.core);
    }

    fn release_scene_cache(&mut self) {
        Widget::release_scene_cache(&mut self.core);
    }

    fn is_layout_dirty(&self) -> bool {
        Widget::is_layout_dirty(&self.core)
    }

    fn layout_needs_measure(&self) -> bool {
        Widget::layout_needs_measure(&self.core)
    }

    fn name(&self) -> &'static str {
        "DataTable"
    }

    fn test_id(&self) -> Option<&str> {
        Widget::test_id(&self.core)
    }

    fn get_rect(&self) -> Rect {
        Widget::get_rect(&self.core)
    }

    fn is_focusable(&self) -> bool {
        Widget::is_focusable(&self.core)
    }

    fn set_focus(&mut self, focused: bool) {
        Widget::set_focus(&mut self.core, focused);
    }

    fn is_focused(&self) -> bool {
        Widget::is_focused(&self.core)
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        Widget::accessibility(&self.core)
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        key_path: &[u64],
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        Widget::perform_action(&mut self.core, action, key_path, state, path)
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        Widget::perform_semantic_action(&mut self.core, target, action, state, path)
    }
}

/// The width a column is laid out at when the band is tight: a resized or
/// fixed width as given, a measured `Auto` width, and the minimum for a
/// flexible column. `None` for a flexible column (and an `Auto` one not yet
/// measured), which takes a share of what is left.
fn column_floor<Row: ?Sized>(
    column: &ColumnDef<Row>,
    overrides: &HashMap<String, f32>,
    auto: &HashMap<String, f32>,
) -> (f32, Option<f32>) {
    let minimum = column.min_width;
    if let Some(px) = overrides.get(&column.key).copied() {
        let width = px.max(minimum);
        return (width, Some(width));
    }
    match column.width {
        ColumnWidth::Fixed(px) => {
            let width = px.max(minimum);
            (width, Some(width))
        }
        ColumnWidth::Auto => auto.get(&column.key).copied().map_or(
            (DataTableCore::MIN_COL_WIDTH.max(minimum), None),
            |px| {
                let width = px.max(minimum);
                (width, Some(width))
            },
        ),
        ColumnWidth::Flex(_) => (DataTableCore::MIN_COL_WIDTH.max(minimum), None),
    }
}

/// Which columns to hide so the rest fit `width`: those with the highest
/// [`ColumnDef::priority`] first, never one at priority `0`.
fn hidden_for_room<Row: ?Sized>(
    columns: &[ColumnDef<Row>],
    width: f64,
    overrides: &HashMap<String, f32>,
    auto: &HashMap<String, f32>,
) -> Vec<bool> {
    let mut hidden = vec![false; columns.len()];
    let floors: Vec<f32> = columns
        .iter()
        .map(|c| column_floor(c, overrides, auto).0)
        .collect();
    let mut order: Vec<usize> = (0..columns.len())
        .filter(|&i| columns.get(i).is_some_and(|c| c.priority > 0))
        .collect();
    order.compact_sort_by_key(|&i| std::cmp::Reverse(columns.get(i).map_or(0, |c| c.priority)));
    let total = |hidden: &[bool]| -> f64 {
        floors
            .iter()
            .zip(hidden)
            .filter(|(_, h)| !**h)
            .map(|(w, _)| f64::from(*w))
            .sum()
    };
    for index in order {
        if total(&hidden) <= width {
            break;
        }
        if let Some(slot) = hidden.get_mut(index) {
            *slot = true;
        }
    }
    hidden
}

/// Allocate weighted space without shrinking a readable flexible minimum.
fn flexible_column_widths<Row: ?Sized>(
    columns: &[ColumnDef<Row>],
    floors: &[(f32, Option<f32>)],
    hidden: &[bool],
    mut available: f64,
) -> Vec<f64> {
    let mut widths = vec![0.0; columns.len()];
    let mut pending: Vec<usize> = (0..columns.len())
        .filter(|&i| !hidden.get(i).copied().unwrap_or(false) && floors[i].1.is_none())
        .collect();
    let weight = |i: usize| match columns[i].width {
        ColumnWidth::Flex(w) => f64::from(w.max(0.0)),
        _ => 1.0,
    };
    while !pending.is_empty() {
        let total: f64 = pending.iter().map(|&i| weight(i)).sum();
        let share = |i: usize| {
            if total > 0.0 {
                available * weight(i) / total
            } else {
                0.0
            }
        };
        let bounds = |i: usize| {
            let floor = f64::from(floors[i].0);
            (floor, f64::from(columns[i].max_width).max(floor))
        };
        let constrained: Vec<(usize, f64)> = pending
            .iter()
            .map(|&i| (i, share(i).clamp(bounds(i).0, bounds(i).1)))
            .filter(|&(i, width)| width.total_cmp(&share(i)).is_ne())
            .collect();
        if constrained.is_empty() {
            for i in pending {
                widths[i] = share(i);
            }
            break;
        }
        for (i, width) in constrained {
            widths[i] = width;
            available = (available - widths[i]).max(0.0);
            pending.retain(|&candidate| candidate != i);
        }
    }
    widths
}

/// Lay the columns out in `host`: fixed, resized and measured `Auto` columns
/// at their widths, flexible columns sharing what is left by weight (or,
/// with none, the `Auto` columns sharing it). When the floors do not fit the
/// columns keep them and the table scrolls sideways. Returns the rects
/// (zero-width for hidden columns) and the columns' total width.
fn compute_column_rects<Row: ?Sized>(
    columns: &[ColumnDef<Row>],
    host: Rect,
    overrides: &HashMap<String, f32>,
    auto: &HashMap<String, f32>,
    hidden: &[bool],
) -> (Vec<Rect>, f64) {
    let shown = |i: usize| !hidden.get(i).copied().unwrap_or(false);
    let total_w = host.width();
    let mut sized = 0.0_f64;
    let mut flex_sum = 0.0_f64;
    let mut floor_sum = 0.0_f64;
    let mut measured_auto = 0_usize;
    for (i, c) in columns.iter().enumerate() {
        if !shown(i) {
            continue;
        }
        let (floor, fixed) = column_floor(c, overrides, auto);
        floor_sum += f64::from(floor);
        match fixed {
            Some(w) => {
                sized += f64::from(w);
                if c.width == ColumnWidth::Auto && !overrides.contains_key(&c.key) {
                    measured_auto += 1;
                }
            }
            None => {
                flex_sum += match c.width {
                    ColumnWidth::Flex(w) => f64::from(w.max(0.0)),
                    _ => 1.0,
                };
            }
        }
    }
    let leftover = (total_w - sized).max(0.0);
    let overflow = floor_sum > total_w + 0.5;
    let floors: Vec<_> = columns
        .iter()
        .map(|c| column_floor(c, overrides, auto))
        .collect();
    let flexible_widths = flexible_column_widths(columns, &floors, hidden, leftover);
    let mut rects = Vec::with_capacity(columns.len());
    let mut x = host.x0;
    let y0 = host.y0;
    let y1 = host.y0 + HEADER_HEIGHT;
    for (i, c) in columns.iter().enumerate() {
        if !shown(i) {
            rects.push(Rect::new(x, y0, x, y1));
            continue;
        }
        let (floor, fixed) = column_floor(c, overrides, auto);
        let w = match fixed {
            _ if overflow => f64::from(floor),
            Some(w) => {
                let auto_share = flex_sum <= 0.0
                    && measured_auto > 0
                    && c.width == ColumnWidth::Auto
                    && !overrides.contains_key(&c.key);
                if auto_share {
                    f64::from(w) + leftover / measured_auto as f64
                } else {
                    f64::from(w)
                }
            }
            None => flexible_widths[i],
        };
        let next = x + w;
        rects.push(Rect::new(x, y0, next, y1));
        x = next;
    }
    (rects, x - host.x0)
}

/// Paint one column header in `cell` (the column's rect at the header's
/// height): `Caption` strong in secondary ink, placed by the column's
/// alignment, with the sort direction as a chevron beside it.
fn paint_header_label(
    ctx: &mut crate::paint::PaintCtx<'_>,
    cell: Rect,
    label: &str,
    align: CellAlign,
    sorted: Option<bool>,
) {
    if cell.width() <= 0.0 {
        return;
    }
    let theme = ctx.theme;
    let tokens = &theme.tokens;
    let text_style = header_style(theme);
    let pad_x = f64::from(tokens.spacing.s2);
    let icon = f64::from(tokens.metrics.icon_sm);
    let icon_gap = f64::from(tokens.spacing.s1);
    let room = (cell.width()
        - 2.0 * pad_x
        - if sorted.is_some() {
            icon + icon_gap
        } else {
            0.0
        })
    .max(0.0);
    let label = ctx
        .text
        .elide_to_width_style(label, f32_of_f64(room), &text_style);
    let (label_w, label_h) = ctx.text.measure_style(&label, None, &text_style);
    let label_y = centered_text_top(cell.y0, cell.height(), label_h);
    let trailing = align == CellAlign::Trailing;
    let label_x = align.place(cell, f64::from(label_w), pad_x);
    ctx.text(
        &label,
        &text_style,
        tokens.colors.text_dim,
        Point::new(label_x, label_y),
        None,
    );
    if let Some(ascending) = sorted {
        // The sort direction is an icon beside the label: before a
        // trailing-aligned numeric header, after a leading one.
        let x0 = if trailing {
            label_x - icon_gap - icon
        } else {
            label_x + f64::from(label_w) + icon_gap
        };
        let glyph =
            Rect::from_center_size(Point::new(x0 + icon / 2.0, cell.center().y), (icon, icon));
        let arrow = if ascending {
            crate::icons::Icon::ChevronUp
        } else {
            crate::icons::Icon::ChevronDown
        };
        arrow.draw(ctx.scene, ctx.transform, glyph, tokens.colors.text_dim);
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FaceColumn<'a> {
    pub header: &'a str,
    pub width: ColumnWidth,
    /// [`TextKind::Numeric`] aligns the column to the trailing edge.
    pub kind: TextKind,
}

impl<'a> FaceColumn<'a> {
    pub fn new(header: &'a str, width: ColumnWidth) -> Self {
        Self {
            header,
            width,
            kind: TextKind::Prose,
        }
    }

    #[must_use]
    pub fn with_kind(mut self, kind: TextKind) -> Self {
        self.kind = kind;
        self
    }
}

/// An immediate-mode table for a painted host (a track list): the
/// [`DataTable`]'s header, columns, row heights, zebra and lift, painted per
/// frame. `paint` takes the table's content rect; the lift and the stripe
/// bleed past it by [`crate::chrome::row_bleed`] as the retained table's do.
/// The host hit-tests with [`Self::row_at`] and scrolls by passing the rows
/// it shows.
#[derive(Clone, Debug)]
pub struct DataTableFace<'a> {
    columns: &'a [FaceColumn<'a>],
    rows: &'a [&'a [&'a str]],
    /// The display index of `rows[0]`, so stripes keep their parity while a
    /// host scrolls.
    first_row: usize,
    selected: Option<usize>,
    hovered: Option<usize>,
    sort: Option<(usize, bool)>,
    compact: bool,
    two_line: bool,
    header: bool,
}

impl<'a> DataTableFace<'a> {
    pub fn new(columns: &'a [FaceColumn<'a>], rows: &'a [&'a [&'a str]]) -> Self {
        Self {
            columns,
            rows,
            first_row: 0,
            selected: None,
            hovered: None,
            sort: None,
            compact: false,
            two_line: false,
            header: true,
        }
    }

    /// The display index of the first row given.
    #[must_use]
    pub fn first_row(mut self, index: usize) -> Self {
        self.first_row = index;
        self
    }

    #[must_use]
    pub fn selected(mut self, row: Option<usize>) -> Self {
        self.selected = row;
        self
    }

    #[must_use]
    pub fn hovered(mut self, row: Option<usize>) -> Self {
        self.hovered = row;
        self
    }

    #[must_use]
    pub fn sorted_by(mut self, column: usize, ascending: bool) -> Self {
        self.sort = Some((column, ascending));
        self
    }

    /// Rows a small control tall.
    #[must_use]
    pub fn compact(mut self) -> Self {
        self.compact = true;
        self
    }

    /// Rows tall enough for a cell the host paints as a `Body` line over a
    /// `Caption` one (a song over its artist, a file over its folder).
    #[must_use]
    pub fn two_line(mut self) -> Self {
        self.two_line = true;
        self
    }

    /// A table without its header band (a continuation of a list whose
    /// columns need no names): rows start at the top of the rect.
    #[must_use]
    pub fn without_header(mut self) -> Self {
        self.header = false;
        self
    }

    /// The header band this face paints: [`Self::header_height`], or none.
    fn header_band(&self, theme: &Theme) -> f64 {
        if self.header {
            Self::header_height(theme)
        } else {
            0.0
        }
    }

    /// The header band's height.
    pub fn header_height(theme: &Theme) -> f64 {
        let tokens = &theme.tokens;
        text_band(
            f64::from(tokens.metrics.min_pointer_target),
            type_line_height(&tokens.typography, TextRole::Caption),
            f64::from(tokens.spacing.s3),
        )
    }

    pub fn row_height(&self, theme: &Theme) -> f64 {
        let tokens = &theme.tokens;
        let floor = table_row_floor(theme, self.compact);
        if self.two_line {
            return text_band(
                floor,
                type_line_height(&tokens.typography, TextRole::Body)
                    + type_line_height(&tokens.typography, TextRole::Caption),
                2.0 * f64::from(tokens.spacing.s2),
            )
            .ceil();
        }
        text_band(
            floor,
            type_line_height(&tokens.typography, TextRole::Caption),
            f64::from(tokens.spacing.s2),
        )
    }

    /// The header and every row given.
    pub fn height(&self, theme: &Theme) -> f64 {
        self.header_band(theme) + self.row_height(theme) * self.rows.len() as f64
    }

    /// Each column's rect in `rect`, at the header's height: the band runs
    /// the bleed past the content edges, `Auto` columns measured from their
    /// header and cells.
    pub fn column_rects(&self, rect: Rect, text: &mut TextRenderer, theme: &Theme) -> Vec<Rect> {
        let bleed = crate::chrome::row_bleed(theme);
        let pad = 2.0 * f64::from(theme.tokens.spacing.s2);
        let mut auto = HashMap::new();
        let defs: Vec<ColumnDef<()>> = self
            .columns
            .iter()
            .enumerate()
            .map(|(i, c)| {
                if c.width == ColumnWidth::Auto {
                    let mut width =
                        f64::from(text.measure_style(c.header, None, &header_style(theme)).0);
                    let style = cell_style(theme, c.kind);
                    for row in self.rows {
                        if let Some(cell) = row.get(i) {
                            width = width.max(f64::from(text.measure_style(cell, None, &style).0));
                        }
                    }
                    auto.insert(i.to_string(), f32_of_f64((width + pad).ceil()));
                }
                ColumnDef::new(i.to_string(), c.header, c.width)
            })
            .collect();
        let band = Rect::new(rect.x0 - bleed, rect.y0, rect.x1 + bleed, rect.y1);
        let hidden = vec![false; defs.len()];
        compute_column_rects(&defs, band, &HashMap::new(), &auto, &hidden)
            .0
            .into_iter()
            .map(|r| Rect::new(r.x0, rect.y0, r.x1, rect.y0 + Self::header_height(theme)))
            .collect()
    }

    /// Row `index`'s lift rect in a table painted into `rect`.
    pub fn row_rect(&self, rect: Rect, index: usize, theme: &Theme) -> Rect {
        let bleed = crate::chrome::row_bleed(theme);
        let h = self.row_height(theme);
        let y0 = rect.y0 + self.header_band(theme) + h * index as f64;
        Rect::new(rect.x0 - bleed, y0, rect.x1 + bleed, y0 + h)
    }

    pub fn row_at(&self, rect: Rect, point: Point, theme: &Theme) -> Option<usize> {
        let top = rect.y0 + self.header_band(theme);
        if point.y < top || point.x < rect.x0 || point.x > rect.x1 {
            return None;
        }
        let index = ((point.y - top) / self.row_height(theme)).floor();
        (index >= 0.0 && (index as usize) < self.rows.len()).then_some(index as usize)
    }

    /// Paint the header and the rows into `rect`.
    pub fn paint(&self, ctx: &mut crate::paint::PaintCtx<'_>, rect: Rect) {
        let theme = ctx.theme;
        let tokens = &theme.tokens;
        let ground = tokens.colors.surfaces.content;
        let columns = self.column_rects(rect, ctx.text, theme);
        let header_h = Self::header_height(theme);
        let bleed = crate::chrome::row_bleed(theme);
        if self.header {
            crate::surface::separator(
                ctx.scene,
                ctx.transform,
                rect.x0 - bleed,
                rect.x1 + bleed,
                rect.y0 + header_h - 0.5,
                0.0,
                ground,
            );
        }
        for (i, (column, cell)) in self
            .columns
            .iter()
            .zip(&columns)
            .enumerate()
            .filter(|_| self.header)
        {
            let sorted = self
                .sort
                .filter(|(sorted, _)| *sorted == i)
                .map(|(_, ascending)| ascending);
            paint_header_label(
                ctx,
                *cell,
                column.header,
                CellAlign::for_kind(column.kind),
                sorted,
            );
        }
        for (index, row) in self.rows.iter().enumerate() {
            let band = self.row_rect(rect, index, theme);
            if band.y0 >= rect.y1 {
                break;
            }
            let selected = self.selected == Some(index);
            let hovered = self.hovered == Some(index);
            if selected || hovered {
                crate::chrome::paint_row_selection(
                    ctx.scene,
                    ctx.transform,
                    band,
                    crate::chrome::SelectionState::new(selected, hovered, false),
                    ground,
                    theme,
                );
            } else if self.first_row.saturating_add(index) % 2 == 1 {
                let radius = f64::from(tokens.radii.item).min(band.height() / 2.0);
                ctx.scene.fill(
                    vello::peniko::Fill::NonZero,
                    ctx.transform,
                    zebra_on(theme, ground),
                    None,
                    &RoundedRect::from_rect(band, radius),
                );
            }
            for ((column, cell), value) in self.columns.iter().zip(&columns).zip(row.iter()) {
                let def = ColumnDef::<()>::new("", "", column.width).with_kind(column.kind);
                paint_cell_text(
                    ctx.scene,
                    ctx.transform,
                    ctx.text,
                    theme,
                    Rect::new(cell.x0, band.y0, cell.x1, band.y1),
                    &def,
                    Some(value),
                );
            }
        }
    }
}

/// How much of the selection lift a zebra stripe takes: a quarter of the
/// way from the ground to the selected row's fill. Taken from the selection
/// itself, a selected row always stands three stripes and more off the
/// ground on every material, the near-black ones included, so selection
/// never reads as one more stripe.
pub(crate) const ZEBRA_SHARE: f32 = 0.25;

/// A zebra stripe on `ground`: [`ZEBRA_SHARE`] of the way to the selection
/// lift. Shared by [`DataTable`] and the block editor's tables.
#[must_use]
pub(crate) fn zebra_on(theme: &Theme, ground: Color) -> Color {
    let selected = crate::chrome::selection_tier_on(
        theme,
        crate::chrome::SelectionState::active(true),
        ground,
    )
    .fill;
    crate::tokens::colors::mix8(ground, selected, ZEBRA_SHARE)
}

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

#[cfg(test)]
mod tier_tests {
    use super::*;
    use crate::surface::edge_delta;
    use crate::tokens::colors::HAIRLINE_MIN_DELTA;

    /// The faintest derived tier these tests exercise.
    const ZEBRA_ALPHA: u8 = 13;

    /// CIE L* of a colour.
    fn lightness(c: Color) -> f64 {
        let y = crate::a11y::relative_luminance(c);
        let f = if y > 216.0 / 24389.0 {
            y.cbrt()
        } else {
            (841.0 / 108.0) * y + 4.0 / 29.0
        };
        116.0 * f - 16.0
    }

    /// A selected row reads past a striped one on every ground of every
    /// shipped material: the selection step is more than three stripe steps,
    /// and the selected row stands clearly off a striped neighbour.
    #[test]
    fn selection_stands_well_past_the_zebra_stripe_on_every_ground() {
        for name in Theme::ALL {
            let theme = Theme::named(name).expect("shipped preset resolves");
            let s = &theme.tokens.colors.surfaces;
            for (ground_name, ground) in [
                ("content", s.content),
                ("sidebar", s.sidebar),
                ("inspector", s.inspector),
                ("raised", s.raised),
            ] {
                let zebra = zebra_on(&theme, ground);
                let selected = crate::chrome::selection_tier_on(
                    &theme,
                    crate::chrome::SelectionState::active(true),
                    ground,
                )
                .fill;
                let stripe = (lightness(zebra) - lightness(ground)).abs();
                let lift = (lightness(selected) - lightness(ground)).abs();
                assert!(
                    lift >= 3.0 * stripe,
                    "{name}/{ground_name}: selection ΔL* {lift:.1} vs stripe ΔL* {stripe:.1}"
                );
                assert!(
                    stripe >= 1.5,
                    "{name}/{ground_name}: the stripe still registers (ΔL* {stripe:.1})"
                );
                let apart = (lightness(selected) - lightness(zebra)).abs();
                assert!(
                    apart >= 6.0,
                    "{name}/{ground_name}: a selected row stands only ΔL* {apart:.1} off a striped one"
                );
            }
        }
    }

    fn presets() -> [Theme; 3] {
        [Theme::dark(), Theme::light(), Theme::high_contrast()]
    }

    #[test]
    fn selected_rows_and_tier_hairlines_register_on_every_ground() {
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

        tier_on_is_plain_hairline_on_ordinary_grounds();
        tiers_keep_their_order_and_register_on_black();
    }
}

/// The least a one-line table row is tall: the theme's single-row height
/// (36, or 28 at compact density, `tokens::Density`). A compact table is a
/// table laid out at compact density whatever its window's density.
fn table_row_floor(theme: &Theme, compact: bool) -> f64 {
    let metrics = &theme.tokens.metrics;
    if compact && theme.tokens.density == crate::tokens::Density::Comfortable {
        let rows = crate::tokens::Density::Compact.rows();
        let comfy = crate::tokens::Density::Comfortable.rows();
        f64::from(metrics.row_single / comfy.single * rows.single)
    } else {
        f64::from(metrics.row_single)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Dispatch;
    use crate::time::Duration;
    use crate::widget::Button;
    use automerge::AutoCommit;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    #[derive(Clone)]
    pub(super) struct R {
        pub(super) name: String,
    }

    #[test]
    fn a_column_kind_styles_its_text_whenever_it_is_declared() {
        for column in [
            ColumnDef::<R>::new("size", "Size", ColumnWidth::Fixed(80.0))
                .with_kind(TextKind::Numeric)
                .with_text(|r: &R| r.name.clone()),
            ColumnDef::<R>::new("size", "Size", ColumnWidth::Fixed(80.0))
                .with_text(|r: &R| r.name.clone())
                .with_kind(TextKind::Numeric),
        ] {
            assert_eq!(column.kind, TextKind::Numeric);
            assert!(column.text.is_some());
        }
    }

    pub(super) fn sample_cols() -> Vec<ColumnDef<R>> {
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

        fn row_state(&self, index: usize) -> Arrival<&Self::Row, &str> {
            self.row_queries
                .set(self.row_queries.get().saturating_add(1));
            if index < self.len() {
                Arrival::Ready(&self.row)
            } else {
                Arrival::Failed("outside model")
            }
        }
    }

    pub(super) struct PagingModel {
        pub(super) row: R,
        pub(super) loaded: Rc<Cell<bool>>,
        pub(super) changed: Rc<Cell<bool>>,
        pub(super) requested: Rc<RefCell<Vec<Range<usize>>>>,
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

        fn row_state(&self, _index: usize) -> Arrival<&Self::Row, &str> {
            Arrival::Ready(&self.row)
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

        fn row_state(&self, _index: usize) -> Arrival<&Self::Row, &str> {
            Arrival::Ready(&self.row)
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

        fn row_state(&self, _index: usize) -> Arrival<&Self::Row, &str> {
            Arrival::Ready(&self.row)
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

        fn row_state(&self, index: usize) -> Arrival<&Self::Row, &str> {
            if index == 3 {
                Arrival::Failed("row failed to load")
            } else if self.loaded.get() {
                Arrival::Ready(&self.row)
            } else {
                Arrival::local()
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
            let (rects, _) =
                compute_column_rects(&cols, host, &HashMap::new(), &HashMap::new(), &[]);
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
                &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
                &automerge::ROOT,
            ));
            assert_eq!(table.model_selection().primary(), Some(&99_999_999));
            table.model_selection_mut().select_all();
            assert!(table.model_selection().is_select_all());
            assert_eq!(table.model_selection().len(100_000_000), 100_000_000);
            assert_eq!(table.model_selection().stored_keys().count(), 0);
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
            t.set_viewport(24.0, f32_of_f64(ROW_HEIGHT));
            t.set_scroll_y(2.0 * f32_of_f64(ROW_HEIGHT) + 5.0);
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
                &AutoCommit::new(),
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
                &AutoCommit::new(),
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
                &AutoCommit::new(),
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

            // Header band is y ∈ [0, 28). Name column starts at x=0. Click (20, 10).
            dispatch(
                &mut t,
                &tree,
                node,
                &WindowEvent::CursorMoved {
                    device_id: winit::event::DeviceId::dummy(),
                    position: (20.0, 10.0).into(),
                },
            );
            dispatch(&mut t, &tree, node, &crate::synthetic_input::mouse_press());

            assert_eq!(*hits.borrow(), vec![0], "header click reported column 0");
            // Sort direction unchanged because the callback replaced toggle.
            assert_eq!(t.sort_state(), Some(("name", true)));
        }

        fn a_sorted_table_speaks_in_the_hosts_rows_and_keeps_its_order() {
            let rows = || {
                vec![
                    R { name: "c".into() },
                    R { name: "a".into() },
                    R { name: "b".into() },
                ]
            };
            let columns = || {
                vec![
                    ColumnDef::new("name", "Name", ColumnWidth::Flex(1.0))
                        .sortable(true)
                        .with_text(|row: &R| row.name.clone()),
                ]
            };
            let state = crate::LocalState::new(None::<TableSort>);
            let clicks = Rc::new(RefCell::new(Vec::new()));
            let activations = Rc::new(RefCell::new(Vec::new()));
            let (click_sink, activate_sink) = (Rc::clone(&clicks), Rc::clone(&activations));
            let mut table = DataTable::new(columns())
                .selectable(true)
                .with_sort_state(&state)
                .on_row_click(move |row, _, _| click_sink.borrow_mut().push(row))
                .on_activate(move |row| activate_sink.borrow_mut().push(row));
            table.set_rows(rows());
            let _ = lay_out(&mut table);
            // The host selects its row 0 ("c"); sorting by the text column
            // moves it to the end and the selection goes with it.
            table.select_row(0);
            assert_eq!(table.toggle_sort(0), Some(("name".into(), true)));
            let shown: Vec<_> = table.view_rows().iter().map(|r| r.name.clone()).collect();
            assert_eq!(shown, ["a", "b", "c"]);
            assert_eq!(table.selected_rows(), [0]);
            assert_eq!(
                state.with(Clone::clone),
                Some(TableSort::ascending("name")),
                "the order is kept where a rebuilt view finds it"
            );

            // A click and a double click on the first row shown name the
            // host's row "a", which is its row 1.
            let _ = lay_out(&mut table);
            let (tree, node) = lay_out(&mut table);
            dispatch(
                &mut table,
                &tree,
                node,
                &WindowEvent::CursorMoved {
                    device_id: winit::event::DeviceId::dummy(),
                    position: (20.0, HEADER_HEIGHT + 12.0).into(),
                },
            );
            for _ in 0..2 {
                for state in [ElementState::Pressed, ElementState::Released] {
                    dispatch(
                        &mut table,
                        &tree,
                        node,
                        &crate::synthetic_input::mouse_input(state, MouseButton::Left),
                    );
                }
            }
            assert_eq!(*clicks.borrow(), [1, 1]);
            assert_eq!(*activations.borrow(), [1]);
            assert_eq!(table.selected_rows(), [1]);

            assert_eq!(table.toggle_sort(0), Some(("name".into(), false)));
            assert_eq!(table.selected_rows(), [1]);
            assert!(table.selection().is_selected(2));

            // A table built again from the same state starts in that order.
            let mut rebuilt = DataTable::new(columns()).with_sort_state(&state);
            rebuilt.set_rows(rows());
            let shown: Vec<_> = rebuilt.view_rows().iter().map(|r| r.name.clone()).collect();
            assert_eq!(shown, ["c", "b", "a"]);
        }

        model_owns_sort_order_while_table_owns_header_state();
        a_sorted_table_speaks_in_the_hosts_rows_and_keeps_its_order();
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
                &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
                &automerge::ROOT,
            ));
            assert_eq!(table.selection().primary(), Some(1));
            assert!(Widget::perform_semantic_action(
                &mut table,
                "row:98:0",
                &crate::a11y::Action::Activate,
                &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
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
            let handled = Widget::perform_action(
                &mut t,
                &crate::a11y::Action::Activate,
                &[7],
                &mut crate::widget::Edits::lend(&mut doc),
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
                    .with_render(Box::new(|_, _, _, _, _, _| {}))
                    .with_accessible_value(|row: &R| format!("custom {}", row.name)),
                ColumnDef::new("action", "Action", ColumnWidth::Fixed(80.0))
                    .with_widget(|_, _| Box::new(Button::new("edit", "Edit"))),
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

    use winit::event::{ElementState, MouseButton};

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
                size: Size::from_lengths(300.0, 400.0),
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
                        Button::new(&id, &row.name)
                            .on_click(move || clicks.set(clicks.get().saturating_add(1))),
                    )
                },
            ),
            ColumnDef::new("name", "Name", ColumnWidth::Flex(1.0)),
        ]
    }

    /// Row actions mount for the row under the pointer only; a table its
    /// host scrolls windows its rows, keeps no bar and leaves the wheel.
    #[test]
    fn row_actions_reveal_under_the_pointer_and_a_host_scrolls_the_rows() {
        let move_to = |table: &mut DataTable<R>, tree: &TaffyTree<()>, node, y: f64| {
            dispatch(
                table,
                tree,
                node,
                &WindowEvent::CursorMoved {
                    device_id: winit::event::DeviceId::dummy(),
                    position: (150.0, y).into(),
                },
            );
        };
        {
            let builds = Rc::new(Cell::new(0));
            let mut columns = interactive_cols(Rc::new(Cell::new(0)), Rc::clone(&builds));
            let actions = columns.remove(0).revealed_on_hover();
            columns.push(actions);
            let mut table = DataTable::new(columns).selectable(true);
            table.set_rows(rows4());
            let (tree, node) = lay_out(&mut table);
            assert_eq!(
                table.mounted_cell_count(),
                0,
                "at rest no row shows actions"
            );
            assert_eq!(table.hovered_row(), None);

            move_to(&mut table, &tree, node, HEADER_HEIGHT + ROW_HEIGHT * 2.5);
            assert_eq!(table.hovered_row(), Some(2));
            assert!(
                table.take_structure_dirty(),
                "the hovered row asks for its actions"
            );
            let (tree, node) = lay_out(&mut table);
            assert_eq!(table.mounted_cell_count(), 1);
            assert_eq!(builds.get(), 1);

            move_to(&mut table, &tree, node, HEADER_HEIGHT + ROW_HEIGHT * 0.5);
            let _ = lay_out(&mut table);
            assert_eq!(table.hovered_row(), Some(0));
            assert_eq!(
                table.mounted_cell_count(),
                1,
                "the actions follow the pointer"
            );
        }
        {
            let mut table = DataTable::new(sample_cols())
                .selectable(true)
                .without_header()
                .host_scrolled();
            table.set_rows(
                (0..200)
                    .map(|i| R {
                        name: i.to_string(),
                    })
                    .collect(),
            );
            assert!((table.content_height() - 200.0 * ROW_HEIGHT).abs() < 0.5);
            table.set_host_window(ROW_HEIGHT * 100.0, ROW_HEIGHT * 5.0);
            let (tree, node) = lay_out(&mut table);
            assert!(
                (table.get_rect().height() - ROW_HEIGHT * 5.0).abs() < 0.5,
                "the table is as tall as the part of it in view, with no header band"
            );
            let (first, last) = table.visible_row_range();
            assert!(
                first <= 100 && last >= 105 && last - first < 20,
                "{first}..{last}"
            );
            assert!(table.scrollbar_thumb().is_none(), "the host owns the bar");
            move_to(&mut table, &tree, node, ROW_HEIGHT * 0.5);
            assert_eq!(
                table.hovered_row(),
                Some(100),
                "rows hit where they are painted"
            );
            let handled = Dispatch::new()
                .layout(&tree, node)
                .send(&mut table, &crate::synthetic_input::mouse_wheel(0.0, -3.0))
                .handled;
            assert!(!handled, "the wheel is the host's");
        }
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
            dispatch(
                &mut table,
                &tree,
                node,
                &WindowEvent::CursorMoved {
                    device_id: winit::event::DeviceId::dummy(),
                    position: (20.0, HEADER_HEIGHT + 12.0).into(),
                },
            );
            dispatch(
                &mut table,
                &tree,
                node,
                &crate::synthetic_input::mouse_press(),
            );
            dispatch(
                &mut table,
                &tree,
                node,
                &crate::synthetic_input::mouse_release(),
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

            // The middle of row 2, under the header.
            dispatch(
                &mut t,
                &tree,
                node,
                &WindowEvent::CursorMoved {
                    device_id: winit::event::DeviceId::dummy(),
                    position: (50.0, HEADER_HEIGHT + 2.5 * ROW_HEIGHT).into(),
                },
            );
            dispatch(&mut t, &tree, node, &crate::synthetic_input::mouse_press());

            assert!(t.selection().is_selected(2), "row 2 selected");
            assert_eq!(t.selection().primary(), Some(2));
            assert_eq!(*clicks.borrow(), vec![(2, false, false)]);
        }

        fn non_selectable_table_ignores_body_clicks() {
            let mut t = DataTable::<R>::new(sample_cols()); // selectable defaults false
            t.set_rows(rows4());
            let (tree, node) = lay_out(&mut t);
            dispatch(
                &mut t,
                &tree,
                node,
                &WindowEvent::CursorMoved {
                    device_id: winit::event::DeviceId::dummy(),
                    position: (50.0, 85.0).into(),
                },
            );
            dispatch(&mut t, &tree, node, &crate::synthetic_input::mouse_press());
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
            let key = |named| {
                crate::synthetic_input::keyboard_input(crate::synthetic_input::key_event(
                    winit::keyboard::Key::Named(named),
                    None,
                    ElementState::Pressed,
                ))
            };
            dispatch(&mut t, &tree, node, &key(NamedKey::ArrowDown)); // → 0
            dispatch(&mut t, &tree, node, &key(NamedKey::ArrowDown)); // → 1
            assert_eq!(t.selection().primary(), Some(1));
            assert!(t.selection().is_selected(1));
        }

        fn aligned_columns_place_their_controls_under_the_header() {
            for (align, expect) in [
                (CellAlign::Leading, 0.0),
                (CellAlign::Center, 0.5),
                (CellAlign::Trailing, 1.0),
            ] {
                let mut table = DataTable::new(vec![
                    ColumnDef::new("action", "Action", ColumnWidth::Fixed(160.0))
                        .with_align(align)
                        .with_widget(|row: &R, index| {
                            Box::new(Button::new(&format!("go-{index}"), &row.name))
                        }),
                    ColumnDef::new("name", "Name", ColumnWidth::Flex(1.0)),
                ])
                .with_row_key(|row: &R| {
                    u64::from(row.name.as_bytes().first().copied().unwrap_or_default())
                });
                table.set_rows(rows4());
                let _ = lay_out(&mut table);
                let _ = lay_out(&mut table);
                let column = table.col_rects[0];
                let control = table.mounted_cells[0].widget.get_rect();
                let slack = column.width() - control.width();
                assert!(
                    align == CellAlign::Leading || slack > 1.0,
                    "a placed control keeps its own width: {control:?} in {column:?}"
                );
                assert!(
                    (control.x0 - column.x0 - slack * expect).abs() < 0.5,
                    "{align:?}: {control:?} in {column:?}"
                );
            }
        }

        interactive_cell_consumes_click_without_selecting_its_row();
        click_selects_row_and_fires_callback();
        non_selectable_table_ignores_body_clicks();
        keyboard_arrows_move_selection_when_focused();
        aligned_columns_place_their_controls_under_the_header();
    }

    fn dispatch(t: &mut DataTable<R>, tree: &TaffyTree<()>, node: NodeId, ev: &WindowEvent) {
        Dispatch::new().layout(tree, node).send(t, ev);
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
                false,
                &mut handled,
            );
            assert!(handled);
            let immediate = table.scroll_y();
            assert_eq!(immediate, 60.0);
            assert!(table.needs_animation_frame());
            let frame = crate::time::now() + crate::time::Duration::from_millis(16);
            // The glide moves only the view: it is no change of the model.
            assert!(!crate::time::testing::with_monotonic_time(frame, || {
                Widget::update(&mut table, &AutoCommit::new(), &automerge::ROOT)
            }));
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
            t.set_viewport(5.0 * f32_of_f64(ROW_HEIGHT), f32_of_f64(ROW_HEIGHT));
            t.set_scroll_y(10.0 * f32_of_f64(ROW_HEIGHT)); // scrolled past 10 rows
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
            t.set_viewport(60.0, f32_of_f64(ROW_HEIGHT));
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
            t.set_viewport(120.0, f32_of_f64(ROW_HEIGHT));
            t.set_scroll_y(50_000.0 * f32_of_f64(ROW_HEIGHT));
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
            assert_eq!(t.scroll_y(), 0.0);
            let wheel = crate::synthetic_input::mouse_wheel(0.0, -1.0);
            dispatch(&mut t, &tree, node, &wheel);
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
        let mut t = DataTable::<R>::new(sample_cols())
            .selectable(true)
            .drag_scroll();
        t.set_rows(
            (0..100)
                .map(|i| R {
                    name: format!("r{i}"),
                })
                .collect(),
        );
        let (tree, node) = lay_out(&mut t);
        t.set_viewport(5.0 * f32_of_f64(ROW_HEIGHT), f32_of_f64(ROW_HEIGHT));
        (t, tree, node)
    }

    /// The selection lift, the stripe and the cells share one edge: the
    /// first column's text (a cell pads it by the bleed) starts on the
    /// table's content edge, the band and its lift stand the bleed past it,
    /// and the live scroll bar keeps its own gutter at the trailing edge.
    #[test]
    fn rows_share_one_edge_and_the_scroll_bar_keeps_its_own_column() {
        let (mut t, _tree, _node) = scrolling_table();
        let _relaid = lay_out(&mut t);
        let bleed = f64::from(Theme::dark().tokens.spacing.s2);
        let gutter = f64::from(Theme::dark().tokens.metrics.scrollbar_gutter);
        let rect = t.get_rect();
        let first = t.col_rects.first().copied().expect("a column");
        let last = t.col_rects.last().copied().expect("a column");
        assert!((first.x0 - (rect.x0 - bleed)).abs() < 1e-6, "{first:?}");
        let track = t.v_track().expect("the bar is live");
        assert!(
            last.x1 <= track.x0 + 1e-6,
            "the last column ends before the bar: {last:?} vs {track:?}"
        );
        let band = t.columns_band();
        assert!((band.x0 - first.x0).abs() < 1e-6);
        assert!((band.x1 - (rect.x1 - gutter)).abs() < 1e-6 && band.x1 <= track.x0);

        let mut plain = DataTable::<R>::new(sample_cols()).selectable(true);
        plain.set_rows(rows4());
        let _laid = lay_out(&mut plain);
        let band = plain.columns_band();
        let rect = plain.get_rect();
        assert!(
            (band.x1 - (rect.x1 + bleed)).abs() < 1e-6,
            "{band:?} in {rect:?}"
        );
    }

    fn bar_x(t: &DataTable<R>) -> f64 {
        t.v_track()
            .map_or(t.get_rect().x1, |track| track.center().x)
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
            t.set_viewport(50.0 * f32_of_f64(ROW_HEIGHT), f32_of_f64(ROW_HEIGHT));
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

            let bottom = t.max_scroll_y();
            t.set_scroll_y(bottom);
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

            dispatch(
                &mut t,
                &tree,
                node,
                &WindowEvent::CursorMoved {
                    device_id: winit::event::DeviceId::dummy(),
                    position: (x, mid_y).into(),
                },
            );
            dispatch(&mut t, &tree, node, &crate::synthetic_input::mouse_press());
            assert!(t.vbar.is_dragging(), "press on thumb begins a drag");
            assert!(t.wants_pointer_capture(), "drag captures the pointer");
            assert_eq!(t.scroll_y(), 0.0);

            // Drag down 50 px → scroll_y increases, staying clamped to max.
            dispatch(
                &mut t,
                &tree,
                node,
                &WindowEvent::CursorMoved {
                    device_id: winit::event::DeviceId::dummy(),
                    position: (x, mid_y + 50.0).into(),
                },
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
                    device_id: winit::event::DeviceId::dummy(),
                    position: (x, mid_y - 50.0).into(),
                },
            );
            assert!(t.scroll_y() < after, "drag up decreases scroll_y");

            dispatch(
                &mut t,
                &tree,
                node,
                &crate::synthetic_input::mouse_release(),
            );
            assert!(!t.vbar.is_dragging(), "release ends the drag");
        }

        fn track_click_pages_scroll_y() {
            let (mut t, tree, node) = scrolling_table();
            let thumb = t.scrollbar_thumb().unwrap();
            let x = bar_x(&t);
            let below_y = thumb.y1 + 5.0;

            dispatch(
                &mut t,
                &tree,
                node,
                &WindowEvent::CursorMoved {
                    device_id: winit::event::DeviceId::dummy(),
                    position: (x, below_y).into(),
                },
            );
            dispatch(&mut t, &tree, node, &crate::synthetic_input::mouse_press());
            assert!(!t.vbar.is_dragging(), "track click is not a drag");
            // Paged by ≈ one viewport (five rows) × PAGE_FRACTION.
            let expected =
                5.0 * f32_of_f64(ROW_HEIGHT) * f32_of_f64(crate::widget::scrollbar::PAGE_FRACTION);
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
            dispatch(
                &mut t,
                &tree,
                node,
                &WindowEvent::CursorMoved {
                    device_id: winit::event::DeviceId::dummy(),
                    position: (x, mid_y).into(),
                },
            );
            dispatch(&mut t, &tree, node, &crate::synthetic_input::mouse_press());
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
            let body_top = table.get_rect().y0 + table.header_h;
            let move_cursor = |y| WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: (20.0, y).into(),
            };
            let button = |state| crate::synthetic_input::mouse_input(state, MouseButton::Left);

            dispatch(&mut table, &tree, node, &move_cursor(body_top + 100.0));
            dispatch(&mut table, &tree, node, &button(ElementState::Pressed));
            assert!(table.wants_pointer_capture());
            dispatch(&mut table, &tree, node, &move_cursor(body_top + 40.0));
            dispatch(&mut table, &tree, node, &button(ElementState::Released));
            assert_eq!(table.scroll_y(), 60.0);
            assert!(
                table.selection().is_empty(),
                "panning does not select a row"
            );

            table.set_scroll_y(0.0);
            dispatch(&mut table, &tree, node, &move_cursor(body_top + 10.0));
            dispatch(&mut table, &tree, node, &button(ElementState::Pressed));
            dispatch(&mut table, &tree, node, &button(ElementState::Released));
            assert!(table.selection().is_selected(0));
        }

        fn reduced_motion_keeps_direct_scroll_without_wheel_or_drag_decay() {
            let (mut table, _tree, node) = scrolling_table();
            table.reduce_motion = true;
            let mut handled = false;
            table.on_wheel(
                &MouseScrollDelta::LineDelta(0.0, -1.0),
                TouchPhase::Moved,
                false,
                &mut handled,
            );
            assert!(handled);
            assert_eq!(table.scroll_y(), 60.0_f32);
            assert!(!table.momentum.is_active());

            let now = crate::time::now();
            let started = now.checked_sub(Duration::from_millis(20)).unwrap_or(now);
            let moved = now.checked_sub(Duration::from_millis(5)).unwrap_or(now);
            let scroll = f64::from(table.scroll_y());
            table.drag_scroll.begin(50.0, scroll, started);
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

    pub(super) fn table_color_fills(t: &mut DataTable<R>) -> usize {
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
                .with_kind(TextKind::Numeric)
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
        assert!(table.adjust_layout_style(&mut tree, node, &mut text));
        let measured = f64::from(table.auto_widths["name"]);
        let cell = f64::from(
            text.measure_style(
                "quarterly-report-final.pdf",
                None,
                &cell_style(&theme, TextKind::Prose),
            )
            .0,
        );
        assert!(measured >= cell, "{measured} fits {cell}");
        assert!(
            !table.adjust_layout_style(&mut tree, node, &mut text),
            "settles"
        );

        // Numeric cells end at the trailing padding; an absent value says so.
        let column = ColumnDef::<File>::new("size", "Size", ColumnWidth::Flex(1.0))
            .with_kind(TextKind::Numeric)
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
        let style = cell_style(&theme, TextKind::Numeric);
        let (w, _) = text.measure_style("1284000", None, &style);
        let pad = f64::from(theme.tokens.spacing.s2);
        // The right-aligned origin lands so the run ends at the padding.
        let x = cell.x1 - pad - f64::from(w);
        assert!(x > cell.center().x, "numbers sit at the trailing edge");
    }
}

#[cfg(test)]
#[path = "data_table_content_edge_tests.rs"]
mod content_edge_tests;

#[path = "data_table_row_motion.rs"]
mod row_motion;

#[cfg(test)]
#[path = "data_table_arrivals_tests.rs"]
mod arrivals_tests;
#[cfg(test)]
#[path = "data_table_motion_tests.rs"]
mod motion_tests;
