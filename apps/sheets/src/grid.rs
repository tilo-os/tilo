//! The spreadsheet grid — a direct-painted view with A1 headers, a
//! scrollable cell matrix, selection (single + range), an in-cell editor,
//! and column/row resize handles directly into the Vello scene.
//!
//! This is intentionally NOT a `ui::Widget`: a spreadsheet needs custom
//! virtualization, per-cell clipping, and pixel-level header chrome that
//! the flexbox-driven widget primitives don't model well. The owning
//! `SheetsApp` widget drives it: it hands the grid a `Rect` + the current
//! cell data each frame, forwards pointer/key events, and applies the
//! [`GridCmd`]s the grid emits back into the CRDT.

use std::collections::{HashMap, HashSet};

use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{Modifiers, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::ColorCompat;
use ui::chart_palette::ChartPalette;
use ui::paint::PaintCtx;
use ui::text_edit::{EditCore, Motion};
use ui::text_render::{TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;
use ui::tokens::colors::ON_FILL_MIN_CONTRAST;

use crate::model::{Addr, CellFormat, CellValue, addr_to_a1, format_value};

pub const ROW_HEADER_W: f64 = 56.0;
pub const COL_HEADER_H: f64 = 28.0;
pub const DEFAULT_COL_W: f64 = 108.0;
pub const DEFAULT_ROW_H: f64 = 26.0;
const MIN_COL_W: f64 = 40.0;
const MIN_ROW_H: f64 = 18.0;
const RESIZE_GRAB: f64 = 4.0;
const CELL_PAD_X: f64 = 6.0;
/// Side length of the fill handle square drawn at the selection's bottom-right
/// corner, and the extra grab margin around it for pointer hit-testing.
const FILL_HANDLE_SIZE: f64 = 8.0;
const FILL_HANDLE_GRAB: f64 = 3.0;
/// Upper bound for an auto-fitted column so one very long value can't blow the
/// column out to an unreadable width (it clips past this, like Excel).
const MAX_AUTOFIT_W: f64 = DEFAULT_COL_W * 3.0;
/// Always-visible, touch-friendly grid scrollbar rail/target thickness.
const SCROLLBAR_THICKNESS: f64 = 16.0;
const SCROLLBAR_MIN_THUMB: f64 = 28.0;
/// Assistive technology receives only the on-screen virtual window. This
/// keeps the semantic tree proportional to a frame, even for a full-size
/// workbook, while the row/column positions below still describe its place in
/// the complete grid.
const A11Y_FALLBACK_COLS: u32 = 8;
const A11Y_FALLBACK_ROWS: u32 = 24;

/// A bounded, rendered slice of the grid for the custom-canvas accessibility
/// adapter. The app turns this into `AccessNode`s because it owns the
/// worksheet's display formatting and document-level semantics.
#[derive(Clone, Debug)]
pub(crate) struct AccessibleGridWindow {
    pub columns: Vec<u32>,
    pub rows: Vec<AccessibleGridRow>,
    pub total_columns: u32,
    pub total_rows: u32,
    pub bounds: Rect,
}

#[derive(Clone, Debug)]
pub(crate) struct AccessibleGridRow {
    pub index: u32,
    pub bounds: Rect,
    pub cells: Vec<AccessibleGridCell>,
}

#[derive(Clone, Debug)]
pub(crate) struct AccessibleGridCell {
    pub addr: Addr,
    pub bounds: Rect,
    pub raw: String,
    pub display: String,
    pub selected: bool,
}

/// A mutation the grid wants applied to the document by the app.
#[derive(Clone, Debug)]
pub enum GridCmd {
    /// Write a cell's raw input (empty string clears it).
    SetCell { addr: Addr, raw: String },
    /// Mark a logical undo checkpoint after a burst of edits.
    Commit,
}

#[derive(Clone)]
struct EditState {
    addr: Addr,
    text: String,
    /// Shared caret/selection core. The grid keeps its custom cell painting
    /// and point-mode formula insertion, but all text editing semantics come
    /// from the same byte-safe core as the other editable surfaces.
    core: EditCore,
}

impl EditState {
    fn text(&self) -> String {
        self.text.clone()
    }

    /// The ordered `(start, end)` char indices of the current selection, or
    /// `None` when nothing is selected.
    fn selection(&self) -> Option<(usize, usize)> {
        let range = self.core.range();
        if range.is_empty() {
            return None;
        }
        Some((
            self.text
                .get(..range.start)
                .map_or(0, |prefix| prefix.chars().count()),
            self.text
                .get(..range.end)
                .map_or(0, |prefix| prefix.chars().count()),
        ))
    }

    fn caret(&self) -> usize {
        self.text
            .get(..self.core.caret().min(self.text.len()))
            .map_or(0, |prefix| prefix.chars().count())
    }

    fn set_caret(&mut self, char_idx: usize, extend: bool) {
        let byte = self
            .text
            .char_indices()
            .nth(char_idx)
            .map_or(self.text.len(), |(byte, _)| byte);
        self.core.place(byte, extend);
    }

    fn apply_edit(&mut self, edit: &ui::text_edit::Edit) {
        if edit.is_noop() {
            return;
        }
        self.text.replace_range(
            edit.at..edit.at.saturating_add(edit.removed),
            &edit.inserted,
        );
        self.core.clamp_to(&self.text);
    }
}

/// One rectangular selection range: an `anchor` corner and the moving
/// `active` corner. A single-cell range has them equal. The full selection
/// is a list of these — Excel/Calc-style discontiguous multi-selection —
/// where the last range is the live one that keyboard/drag extend.
#[derive(Clone, Copy, PartialEq)]
struct SelRange {
    anchor: Addr,
    active: Addr,
}

impl SelRange {
    /// Normalized inclusive bounds `(c0, r0, c1, r1)` with `top_left <= bottom_right`.
    fn bounds(&self) -> (u32, u32, u32, u32) {
        (
            self.anchor.0.min(self.active.0),
            self.anchor.1.min(self.active.1),
            self.anchor.0.max(self.active.0),
            self.anchor.1.max(self.active.1),
        )
    }
    fn spans_col(&self, c: u32) -> bool {
        let (c0, _, c1, _) = self.bounds();
        c >= c0 && c <= c1
    }
    fn spans_row(&self, r: u32) -> bool {
        let (_, r0, _, r1) = self.bounds();
        r >= r0 && r <= r1
    }
}

#[derive(Clone, Copy)]
enum ResizeTarget {
    Col(u32),
    Row(u32),
}

/// A caret motion within the in-cell / formula-bar text editor.
#[derive(Clone, Copy)]
enum CaretMove {
    Left,
    Right,
    Home,
    End,
}

struct ResizeDrag {
    target: ResizeTarget,
    origin_px: f64,
    origin_size: f64,
}

#[derive(Clone, Copy)]
enum ScrollAxis {
    Horizontal,
    Vertical,
}

struct ScrollDrag {
    axis: ScrollAxis,
    pointer_origin: f64,
    scroll_origin: f64,
}

/// An in-flight fill-handle drag. The selection at grab time is the source
/// *pattern*; `target` is the cell currently under the pointer, projected onto
/// the dominant forward axis (down or right) to a fill region.
#[derive(Clone, Copy)]
struct FillDrag {
    /// Source selection bounds captured at grab, `(top_left, bottom_right)`.
    src: (Addr, Addr),
    /// Cell currently under the pointer.
    target: Addr,
}

/// Text and band metrics for the grid, resolved from the renderer that paints
/// it.
///
/// Theme typography tokens already carry the accessibility text scale and the
/// renderer multiplies its input by that scale once more, so the app converts
/// each token through [`TextRenderer::renderer_input_style`] exactly once and
/// hands the grid the result. The column-header height, the row-header width
/// and the *default* row height then derive from the measured line box of
/// those styles, so layout, paint and hit-testing share one metric. Manual row
/// heights and column widths are never touched. At 1× the floors reproduce the
/// classic 26 px row / 28 px header / 56 px row-header density exactly.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridMetrics {
    /// Accessibility text scale used to grow authored interaction floors.
    pub scale: f64,
    /// Renderer-input size for cell text (theme `base` ÷ renderer scale).
    pub cell_size: f32,
    /// Renderer-input size for header labels (theme `sm` ÷ renderer scale).
    pub header_size: f32,
    /// Measured line box of `cell_size` text, in final pixels.
    pub cell_line_h: f64,
    /// Measured line box of `header_size` text, in final pixels.
    pub header_line_h: f64,
    /// Measured width of a four-digit row label at `header_size`.
    pub row_label_w: f64,
    /// Theme spacing tokens the bands pad with (`s2`, `s3`).
    pub pad: f64,
    pub header_pad: f64,
}

impl GridMetrics {
    /// Resolve the grid metrics for `theme` against `tr`, the renderer that
    /// will paint the grid.
    pub fn resolve(tr: &mut TextRenderer, theme: &Theme) -> Self {
        let scale = &theme.tokens.typography.scale;
        let cell = tr.renderer_input_style(&TextStyle::new(scale.base));
        let header = tr.renderer_input_style(&TextStyle::new(scale.sm));
        let (_, cell_line_h) = tr.measure_style("Ag", None, &cell);
        let (row_label_w, header_line_h) = tr.measure_style("9999", None, &header);
        Self {
            scale: f64::from(theme.text_scale),
            cell_size: cell.size,
            header_size: header.size,
            cell_line_h: f64::from(cell_line_h),
            header_line_h: f64::from(header_line_h),
            row_label_w: f64::from(row_label_w),
            pad: f64::from(theme.tokens.spacing.s2),
            header_pad: f64::from(theme.tokens.spacing.s3),
        }
    }

    /// The 1× metrics used before the first draw has a renderer; every band
    /// lands on its classic floor.
    #[must_use]
    pub fn fallback() -> Self {
        Self {
            scale: 1.0,
            cell_size: 14.0,
            header_size: 12.0,
            cell_line_h: 17.5,
            header_line_h: 15.0,
            row_label_w: 28.0,
            pad: 8.0,
            header_pad: 12.0,
        }
    }

    /// Height of an automatically sized row: the cell line box plus `s2`,
    /// never below the classic 26 px.
    #[must_use]
    pub fn default_row_h(&self) -> f64 {
        (self.cell_line_h + self.pad).max(DEFAULT_ROW_H * self.scale)
    }

    /// Height of the column-header strip: the header line box plus `s3`,
    /// never below the classic 28 px.
    #[must_use]
    pub fn col_header_h(&self) -> f64 {
        (self.header_line_h + self.header_pad).max(COL_HEADER_H * self.scale)
    }

    /// Width of the row-header strip: a four-digit label plus `s2` each side,
    /// never below the classic 56 px.
    #[must_use]
    pub fn row_header_w(&self) -> f64 {
        (self.row_label_w + 2.0 * self.pad).max(ROW_HEADER_W * self.scale)
    }
}

impl Default for GridMetrics {
    fn default() -> Self {
        Self::fallback()
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "selecting/point_selecting/show_formulas/needs_autofit/dirty are independent \
              interaction and render-invalidation flags, not exclusive states an enum would \
              simplify; consolidating them would touch this widget's many call sites for no \
              behavioural gain"
)]
pub struct Grid {
    pub rect: Rect,
    /// Geometry follows the active accessibility text scale. The grid is a
    /// custom canvas rather than a toolkit table, so it must grow its rows,
    /// headers, hit targets, and default columns alongside token typography.
    text_scale: f64,
    scroll_x: f64,
    scroll_y: f64,
    col_widths: HashMap<u32, f64>,
    /// Widths derived by fitting a column to its content (see
    /// `autofit_columns`). A manual `col_widths` entry always overrides this;
    /// columns with neither fall back to `DEFAULT_COL_W`.
    auto_col_w: HashMap<u32, f64>,
    row_heights: HashMap<u32, f64>,
    n_cols: u32,
    n_rows: u32,
    /// Prefix sums of column/row extents so `col_left`/`row_top` are O(1)
    /// instead of a per-cell accumulate loop. `col_x[c]` is the content-space
    /// x of column `c`'s left edge; `col_x[n_cols]` is the total width.
    /// Rebuilt only when sizes or the logical extent change.
    col_x: Vec<f64>,
    row_y: Vec<f64>,
    /// The live selection range's anchor + active cell. A single-cell
    /// selection has them equal. `active` is the cell keyboard nav moves and
    /// the formula bar reflects; it always stays visible within the selection.
    anchor: Addr,
    active: Addr,
    /// Committed discontiguous ranges added via Ctrl+click / Ctrl+drag. The
    /// full selection is these plus the live `anchor..active` range.
    extra_ranges: Vec<SelRange>,
    edit: Option<EditState>,
    values: HashMap<Addr, CellValue>,
    raw: HashMap<Addr, String>,
    fmt: HashMap<Addr, CellFormat>,
    bold: HashSet<Addr>,
    last_cursor: Point,
    resize: Option<ResizeDrag>,
    scroll_drag: Option<ScrollDrag>,
    /// An in-flight fill-handle drag (grab the selection's bottom-right corner
    /// and drag down/right to extend the pattern).
    fill_drag: Option<FillDrag>,
    /// A committed fill the app consumes on the next event: the source pattern
    /// bounds and the destination region to fill, `(src_tl, src_br, dst_tl,
    /// dst_br)`. Rising-edge, cleared by `take_pending_fill`.
    pending_fill: Option<(Addr, Addr, Addr, Addr)>,
    selecting: bool,
    /// Formula "point mode": while editing a formula, clicking/dragging cells
    /// inserts their references. `point_anchor` is the char index in the edit
    /// buffer where the last click-inserted reference begins — a subsequent
    /// click/drag (with nothing typed in between) replaces it, matching Excel.
    /// Cleared to `None` the moment the user types.
    point_anchor: Option<usize>,
    /// The cell a point-mode drag started on, so the range grows from it.
    point_drag_start: Addr,
    /// Whether a point-mode drag is in progress (a held click after pointing).
    point_selecting: bool,
    /// "Formula view": when on, cells that hold a raw input show that raw text
    /// (formulas verbatim) instead of their computed / formatted value.
    show_formulas: bool,
    /// Set when cell data changes so the next `draw` recomputes auto-fit
    /// widths (needs a `TextRenderer`, only available at draw time).
    needs_autofit: bool,
    dirty: bool,
    /// Resolved text/band metrics (see [`GridMetrics`]). Set by the app once
    /// per draw from the renderer; `fallback()` until then.
    metrics: GridMetrics,
}

impl Default for Grid {
    fn default() -> Self {
        Self::new()
    }
}

impl Grid {
    pub fn new() -> Self {
        let mut g = Self {
            rect: Rect::ZERO,
            text_scale: 1.0,
            scroll_x: 0.0,
            scroll_y: 0.0,
            col_widths: HashMap::new(),
            auto_col_w: HashMap::new(),
            row_heights: HashMap::new(),
            n_cols: 48,
            n_rows: 200,
            col_x: Vec::new(),
            row_y: Vec::new(),
            anchor: (0, 0),
            active: (0, 0),
            extra_ranges: Vec::new(),
            edit: None,
            values: HashMap::new(),
            raw: HashMap::new(),
            fmt: HashMap::new(),
            bold: HashSet::new(),
            last_cursor: Point::ZERO,
            resize: None,
            scroll_drag: None,
            fill_drag: None,
            pending_fill: None,
            selecting: false,
            point_anchor: None,
            point_drag_start: (0, 0),
            point_selecting: false,
            show_formulas: false,
            needs_autofit: true,
            dirty: true,
            metrics: GridMetrics::fallback(),
        };
        g.rebuild_geom();
        g
    }

    /// Adopt freshly resolved metrics. A change re-derives the automatic row
    /// geometry (manual sizes are untouched) and re-fits columns, since both
    /// depend on the cell text size.
    pub fn set_text_scale(&mut self, scale: f64) {
        let scale = if scale.is_finite() {
            scale.clamp(0.5, 4.0)
        } else {
            1.0
        };
        if (self.text_scale - scale).abs() <= f64::EPSILON {
            return;
        }
        self.text_scale = scale;
        self.needs_autofit = true;
        self.rebuild_geom();
        self.scroll_x = self.scroll_x.min(self.max_scroll_x());
        self.scroll_y = self.scroll_y.min(self.max_scroll_y());
    }

    pub fn set_metrics(&mut self, metrics: GridMetrics) {
        if self.metrics == metrics {
            return;
        }
        self.metrics = metrics;
        self.rebuild_geom();
        // Shorter rows shrink the content extent; keep the scroll inside it so
        // the body never paints empty past the last row.
        self.clamp_scroll();
        self.needs_autofit = true;
        self.dirty = true;
    }

    /// The metrics currently driving the grid's automatic geometry.
    #[must_use]
    pub fn metrics(&self) -> GridMetrics {
        self.metrics
    }

    /// Thickness of the always-visible scrollbar rails along the body's right
    /// and bottom edges, so chrome floated over the grid can stay clear of them.
    #[must_use]
    pub fn scrollbar_thickness() -> f64 {
        SCROLLBAR_THICKNESS
    }

    /// Recompute the column/row offset prefix sums. Cheap (a single pass over
    /// the logical extent) and only called when a size or the extent changes,
    /// so per-frame geometry lookups stay O(1).
    fn rebuild_geom(&mut self) {
        self.col_x.clear();
        self.col_x.reserve(self.n_cols as usize + 1);
        let mut x = 0.0;
        self.col_x.push(0.0);
        for c in 0..self.n_cols {
            x += self.col_w(c);
            self.col_x.push(x);
        }
        self.row_y.clear();
        self.row_y.reserve(self.n_rows as usize + 1);
        let mut y = 0.0;
        self.row_y.push(0.0);
        for r in 0..self.n_rows {
            y += self.row_h(r);
            self.row_y.push(y);
        }
    }

    // --- data plumbing ------------------------------------------------------

    /// Replace the rendered cell data (called every frame from the app's
    /// `sync` after recalculation).
    pub fn set_data(
        &mut self,
        raw: HashMap<Addr, String>,
        fmt: HashMap<Addr, CellFormat>,
        values: HashMap<Addr, CellValue>,
    ) {
        // Grow the logical extent so headers cover all populated cells.
        let (mut nc, mut nr) = (self.n_cols, self.n_rows);
        for &(c, r) in raw.keys() {
            nc = nc.max(c + 4);
            nr = nr.max(r + 8);
        }
        let grew = nc != self.n_cols || nr != self.n_rows;
        self.n_cols = nc;
        self.n_rows = nr;
        self.raw = raw;
        self.fmt = fmt;
        self.values = values;
        if grew {
            self.rebuild_geom();
        }
        // Content changed → refit columns to it on the next draw.
        self.needs_autofit = true;
    }

    /// Replace the set of cells drawn in bold.
    pub fn set_bold(&mut self, bold: HashSet<Addr>) {
        if self.bold != bold {
            self.bold = bold;
            self.needs_autofit = true;
        }
        self.dirty = true;
    }

    /// Toggle "formula view" (show raw formulas instead of results). Returns
    /// the new state so the caller can surface it in a status line.
    pub fn toggle_formula_view(&mut self) -> bool {
        self.show_formulas = !self.show_formulas;
        self.dirty = true;
        self.show_formulas
    }

    /// Whether formula view is currently on.
    pub fn formula_view(&self) -> bool {
        self.show_formulas
    }

    pub fn dirty(&self) -> bool {
        self.dirty
    }
    pub fn mark_drawn(&mut self) {
        self.dirty = false;
    }
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub fn active(&self) -> Addr {
        self.active
    }

    /// Set the selection rectangle directly. Used by the app's tests to drive
    /// the real selection-summary path without synthesizing pointer events
    /// (winit's `DeviceId` has no public constructor).
    #[cfg(test)]
    pub(crate) fn set_selection(&mut self, anchor: Addr, active: Addr) {
        self.extra_ranges.clear();
        self.anchor = anchor;
        self.active = active;
    }

    /// Collapse the selection onto a single cell (used on sheet switch and
    /// for the seeded screenshot selection).
    pub fn select_only(&mut self, addr: Addr) {
        let (nc, nr) = (self.n_cols.max(addr.0 + 4), self.n_rows.max(addr.1 + 8));
        if nc != self.n_cols || nr != self.n_rows {
            self.n_cols = nc;
            self.n_rows = nr;
            self.rebuild_geom();
        }
        self.extra_ranges.clear();
        self.anchor = addr;
        self.active = addr;
        self.edit = None;
        self.ensure_visible();
        self.dirty = true;
    }

    /// Select a rectangular range (anchor..active), replacing any existing
    /// selection. Used by paste (to select what landed) and the shots hooks.
    pub fn select_range_public(&mut self, anchor: Addr, active: Addr) {
        let (nc, nr) = (
            self.n_cols.max(anchor.0.max(active.0) + 4),
            self.n_rows.max(anchor.1.max(active.1) + 8),
        );
        if nc != self.n_cols || nr != self.n_rows {
            self.n_cols = nc;
            self.n_rows = nr;
            self.rebuild_geom();
        }
        self.extra_ranges.clear();
        self.anchor = anchor;
        self.active = active;
        self.edit = None;
        self.ensure_visible();
        self.dirty = true;
    }

    /// Add a discontiguous rectangular range to the current selection,
    /// keeping the existing one(s). The new range becomes the live one.
    /// Drives the multi-range shots hook (Ctrl+drag equivalent).
    pub fn add_range_public(&mut self, anchor: Addr, active: Addr) {
        let (nc, nr) = (
            self.n_cols.max(anchor.0.max(active.0) + 4),
            self.n_rows.max(anchor.1.max(active.1) + 8),
        );
        if nc != self.n_cols || nr != self.n_rows {
            self.n_cols = nc;
            self.n_rows = nr;
            self.rebuild_geom();
        }
        self.extra_ranges.push(self.current_range());
        self.anchor = anchor;
        self.active = active;
        self.edit = None;
        self.ensure_visible();
        self.dirty = true;
    }

    /// Public hit-test wrapper so the app can detect double-clicks.
    pub fn cell_at_public(&self, p: Point) -> Option<Addr> {
        self.cell_at(p)
    }

    pub fn is_editing(&self) -> bool {
        self.edit.is_some()
    }

    /// What the formula bar should show: the live edit buffer if editing,
    /// otherwise the active cell's stored raw input.
    pub fn active_display_raw(&self) -> String {
        if let Some(e) = &self.edit
            && e.addr == self.active
        {
            return e.text();
        }
        self.raw.get(&self.active).cloned().unwrap_or_default()
    }

    /// Return the viewport-sized semantic model for screen readers. Unlike a
    /// naive table export, this never walks every row/cell in a workbook: an
    /// unopened million-row sheet contributes only the visible window. Before
    /// the first layout, retain a small window around the active cell so an AT
    /// can still describe and focus the current cell during startup.
    pub(crate) fn accessibility_window(&self) -> AccessibleGridWindow {
        let columns = if self.has_layout() {
            self.visible_cols()
        } else {
            bounded_window(self.active.0, self.n_cols, A11Y_FALLBACK_COLS)
        };
        let row_indices = if self.has_layout() {
            self.visible_rows()
        } else {
            bounded_window(self.active.1, self.n_rows, A11Y_FALLBACK_ROWS)
        };
        let rows = row_indices
            .into_iter()
            .map(|row| {
                let cells = columns
                    .iter()
                    .copied()
                    .map(|col| {
                        let addr = (col, row);
                        let raw = self.raw.get(&addr).cloned().unwrap_or_default();
                        let display = if self.show_formulas && !raw.is_empty() {
                            raw.clone()
                        } else {
                            self.values.get(&addr).map_or_else(String::new, |value| {
                                format_value(
                                    value,
                                    self.fmt.get(&addr).copied().unwrap_or_default(),
                                )
                            })
                        };
                        AccessibleGridCell {
                            addr,
                            bounds: self.cell_screen_rect(col, row),
                            raw,
                            display,
                            selected: self.cell_is_selected(addr),
                        }
                    })
                    .collect();
                AccessibleGridRow {
                    index: row,
                    bounds: Rect::new(
                        self.rect.x0,
                        self.cell_screen_rect(0, row).y0,
                        self.rect.x1,
                        self.cell_screen_rect(0, row).y1,
                    ),
                    cells,
                }
            })
            .collect();
        AccessibleGridWindow {
            columns,
            rows,
            total_columns: self.n_cols,
            total_rows: self.n_rows,
            bounds: self.rect,
        }
    }

    fn cell_is_selected(&self, addr: Addr) -> bool {
        self.extra_ranges
            .iter()
            .copied()
            .chain(std::iter::once(self.current_range()))
            .any(|range| {
                let (c0, r0, c1, r1) = range.bounds();
                addr.0 >= c0 && addr.0 <= c1 && addr.1 >= r0 && addr.1 <= r1
            })
    }

    /// The live (in-progress / most-recent) selection range.
    fn current_range(&self) -> SelRange {
        SelRange {
            anchor: self.anchor,
            active: self.active,
        }
    }

    /// Every selection range: the committed discontiguous ones plus the live
    /// `anchor..active`. The live range is always last.
    fn all_ranges(&self) -> Vec<SelRange> {
        let mut v = self.extra_ranges.clone();
        v.push(self.current_range());
        v
    }

    /// True when more than one discontiguous range is selected. Copy widens
    /// its status hint and the name box shows a cell count in this case.
    pub fn is_multi_range(&self) -> bool {
        !self.extra_ranges.is_empty()
    }

    /// Whether column `c` intersects any selected range (drives the header
    /// highlight — a column header lights up when it holds a selected cell).
    fn col_in_selection(&self, c: u32) -> bool {
        self.current_range().spans_col(c) || self.extra_ranges.iter().any(|rg| rg.spans_col(c))
    }
    fn row_in_selection(&self, r: u32) -> bool {
        self.current_range().spans_row(r) || self.extra_ranges.iter().any(|rg| rg.spans_row(r))
    }

    /// Every cell across all selected ranges, de-duplicated so overlapping
    /// ranges don't double-apply a format or double-count a sum. Copy, fill,
    /// formatting, delete, and the status-bar summary all operate over this.
    pub fn selected_cells(&self) -> Vec<Addr> {
        // Fast path: a single contiguous range can't overlap itself, so the
        // common case (which runs every interaction frame via the status-bar
        // summary) skips the dedup set entirely.
        if self.extra_ranges.is_empty() {
            let (c0, r0, c1, r1) = self.current_range().bounds();
            let mut out = Vec::with_capacity(((c1 - c0 + 1) as usize) * ((r1 - r0 + 1) as usize));
            for r in r0..=r1 {
                for c in c0..=c1 {
                    out.push((c, r));
                }
            }
            return out;
        }
        // Discontiguous selection: dedup across possibly-overlapping ranges.
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for range in self.all_ranges() {
            let (c0, r0, c1, r1) = range.bounds();
            for r in r0..=r1 {
                for c in c0..=c1 {
                    if seen.insert((c, r)) {
                        out.push((c, r));
                    }
                }
            }
        }
        out
    }

    /// The bounding box over every selected range, as `(top_left,
    /// bottom_right)`, inclusive and normalized. Drives copy/cut and fill.
    pub fn selection_bounds(&self) -> (Addr, Addr) {
        let mut ranges = self.all_ranges().into_iter();
        let (mut c0, mut r0, mut c1, mut r1) =
            ranges.next().map(|r| r.bounds()).unwrap_or_default();
        for range in ranges {
            let (a, b, cc, d) = range.bounds();
            c0 = c0.min(a);
            r0 = r0.min(b);
            c1 = c1.max(cc);
            r1 = r1.max(d);
        }
        ((c0, r0), (c1, r1))
    }

    /// The stored raw input for a cell (empty when unset). Used by copy/cut.
    pub fn cell_raw_at(&self, addr: Addr) -> String {
        self.raw.get(&addr).cloned().unwrap_or_default()
    }

    /// The effective format for a cell (the propagated/explicit one the app
    /// pushed in via `set_data`). Used by copy so paste can restore it.
    pub fn cell_fmt_at(&self, addr: Addr) -> CellFormat {
        self.fmt.get(&addr).copied().unwrap_or_default()
    }

    /// The computed value for a cell, if any. Used to build the plain-text
    /// clipboard payload (what other apps paste as).
    pub fn cell_value_at(&self, addr: Addr) -> Option<CellValue> {
        self.values.get(&addr).cloned()
    }

    /// Select the populated data range (the bounding box of every non-empty
    /// cell), collapsing onto A1 when the sheet is empty. Drives Ctrl+A.
    pub fn select_used_range(&mut self) {
        let mut it = self
            .raw
            .iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(a, _)| *a);
        if let Some(first) = it.next() {
            let (mut c0, mut r0, mut c1, mut r1) = (first.0, first.1, first.0, first.1);
            for (c, r) in it {
                c0 = c0.min(c);
                r0 = r0.min(r);
                c1 = c1.max(c);
                r1 = r1.max(r);
            }
            self.extra_ranges.clear();
            // Keep the active cell at the top-left so the view doesn't jump to
            // the bottom-right corner of a large used range.
            self.anchor = (c1, r1);
            self.active = (c0, r0);
            self.edit = None;
            self.ensure_visible();
            self.dirty = true;
        } else {
            self.select_only((0, 0));
        }
    }

    /// A1-style label for the selection (`B2`, `B2:D5`, or an `N cells` count
    /// for a discontiguous multi-range selection — like the name box in Excel).
    pub fn selection_label(&self) -> String {
        if !self.extra_ranges.is_empty() {
            return format!("{} cells", self.selected_cells().len());
        }
        if self.anchor == self.active {
            addr_to_a1(self.active.0, self.active.1)
        } else {
            let (c0, c1) = (
                self.anchor.0.min(self.active.0),
                self.anchor.0.max(self.active.0),
            );
            let (r0, r1) = (
                self.anchor.1.min(self.active.1),
                self.anchor.1.max(self.active.1),
            );
            format!("{}:{}", addr_to_a1(c0, r0), addr_to_a1(c1, r1))
        }
    }

    // --- geometry helpers ---------------------------------------------------

    fn col_w(&self, c: u32) -> f64 {
        // A manual resize wins; otherwise use the auto-fitted width; otherwise
        // the default.
        if let Some(w) = self.col_widths.get(&c) {
            return *w * self.text_scale;
        }
        self.auto_col_w
            .get(&c)
            .copied()
            .unwrap_or(DEFAULT_COL_W * self.text_scale)
    }
    fn row_h(&self, r: u32) -> f64 {
        self.row_heights
            .get(&r)
            .copied()
            .unwrap_or_else(|| self.metrics.default_row_h())
    }

    /// Content-space x (before scroll) of column `c`'s left edge. O(1) via
    /// the cached prefix sums, with a defensive fallback past the last index.
    fn col_left(&self, c: u32) -> f64 {
        if let Some(&x) = self.col_x.get(c as usize) {
            x
        } else {
            let last = self.col_x.last().copied().unwrap_or(0.0);
            last + f64::from(c.saturating_sub(self.n_cols)) * DEFAULT_COL_W * self.text_scale
        }
    }
    fn row_top(&self, r: u32) -> f64 {
        if let Some(&y) = self.row_y.get(r as usize) {
            y
        } else {
            let last = self.row_y.last().copied().unwrap_or(0.0);
            last + f64::from(r.saturating_sub(self.n_rows)) * self.metrics.default_row_h()
        }
    }

    fn body_rect(&self) -> Rect {
        Rect::new(
            self.grid_x0(),
            self.grid_y0(),
            self.rect.x1 - SCROLLBAR_THICKNESS,
            self.rect.y1 - SCROLLBAR_THICKNESS,
        )
    }

    fn max_scroll_x(&self) -> f64 {
        (self.total_width() - self.body_rect().width()).max(0.0)
    }

    fn max_scroll_y(&self) -> f64 {
        (self.total_height() - self.body_rect().height()).max(0.0)
    }

    fn horizontal_scrollbar_rect(&self) -> Rect {
        let body = self.body_rect();
        Rect::new(body.x0, body.y1, body.x1, self.rect.y1)
    }

    fn vertical_scrollbar_rect(&self) -> Rect {
        let body = self.body_rect();
        Rect::new(body.x1, body.y0, self.rect.x1, body.y1)
    }

    fn scrollbar_thumb(&self, axis: ScrollAxis) -> Rect {
        let (rail, content, viewport, scroll) = match axis {
            ScrollAxis::Horizontal => (
                self.horizontal_scrollbar_rect(),
                self.total_width(),
                self.body_rect().width(),
                self.scroll_x,
            ),
            ScrollAxis::Vertical => (
                self.vertical_scrollbar_rect(),
                self.total_height(),
                self.body_rect().height(),
                self.scroll_y,
            ),
        };
        let length = match axis {
            ScrollAxis::Horizontal => rail.width(),
            ScrollAxis::Vertical => rail.height(),
        };
        let cross = match axis {
            ScrollAxis::Horizontal => rail.height(),
            ScrollAxis::Vertical => rail.width(),
        };
        let thumb = if content <= viewport {
            length
        } else {
            (length * viewport / content)
                .max(SCROLLBAR_MIN_THUMB * self.text_scale)
                .min(length)
        };
        let max_scroll = (content - viewport).max(0.0);
        let offset = if max_scroll <= 0.0 {
            0.0
        } else {
            (length - thumb) * scroll / max_scroll
        };
        match axis {
            ScrollAxis::Horizontal => Rect::new(
                rail.x0 + offset,
                rail.y0 + 2.0,
                rail.x0 + offset + thumb,
                rail.y0 + cross - 2.0,
            ),
            ScrollAxis::Vertical => Rect::new(
                rail.x0 + 2.0,
                rail.y0 + offset,
                rail.x0 + cross - 2.0,
                rail.y0 + offset + thumb,
            ),
        }
    }

    fn grid_x0(&self) -> f64 {
        self.rect.x0 + self.metrics.row_header_w()
    }
    fn grid_y0(&self) -> f64 {
        self.rect.y0 + self.metrics.col_header_h()
    }

    /// Screen rect for a cell, in absolute coords (may lie outside the body).
    fn cell_screen_rect(&self, c: u32, r: u32) -> Rect {
        let x = self.grid_x0() + self.col_left(c) - self.scroll_x;
        let y = self.grid_y0() + self.row_top(r) - self.scroll_y;
        Rect::new(x, y, x + self.col_w(c), y + self.row_h(r))
    }

    /// Screen rect of the fill handle — a small square centred on the live
    /// selection's bottom-right corner. Shown only for a single contiguous
    /// selection that is not being edited, and only while the corner is on the
    /// visible body. `None` otherwise.
    fn fill_handle_rect(&self) -> Option<Rect> {
        if self.edit.is_some() || !self.extra_ranges.is_empty() {
            return None;
        }
        let (_, _, c1, r1) = self.current_range().bounds();
        let cell = self.cell_screen_rect(c1, r1);
        let (cx, cy) = (cell.x1, cell.y1);
        let body = self.body_rect();
        let m = FILL_HANDLE_SIZE * self.text_scale;
        if cx < body.x0 - m || cx > body.x1 + m || cy < body.y0 - m || cy > body.y1 + m {
            return None;
        }
        let h = FILL_HANDLE_SIZE * self.text_scale / 2.0;
        Some(Rect::new(cx - h, cy - h, cx + h, cy + h))
    }

    /// Project a fill-handle drag onto the dominant **forward** axis (down or
    /// right) and return `(src_tl, src_br, dst_tl, dst_br)`, or `None` when the
    /// pointer has not left the source (no extension yet). Dragging beyond the
    /// bottom fills down; beyond the right fills right; a diagonal snaps to the
    /// axis with the larger overshoot.
    fn fill_region(src: (Addr, Addr), target: Addr) -> Option<(Addr, Addr, Addr, Addr)> {
        let ((c0, r0), (c1, r1)) = src;
        let (tc, tr) = target;
        let down = i64::from(tr) - i64::from(r1);
        let right = i64::from(tc) - i64::from(c1);
        if down <= 0 && right <= 0 {
            return None;
        }
        if down > 0 && down >= right {
            Some(((c0, r0), (c1, r1), (c0, r1 + 1), (c1, tr)))
        } else {
            Some(((c0, r0), (c1, r1), (c1 + 1, r0), (tc, r1)))
        }
    }

    /// Consume a committed fill-handle drag: `(src_tl, src_br, dst_tl, dst_br)`.
    /// The app extends the source pattern over the destination region.
    pub fn take_pending_fill(&mut self) -> Option<(Addr, Addr, Addr, Addr)> {
        self.pending_fill.take()
    }

    /// Drive a fill-handle drag from the current selection to `target` and
    /// release, exactly as a pointer grab + drag + release would — the
    /// production `pointer_up` path records the pending fill. Used by tests and
    /// the `--shots` scenarios to exercise the real fill without a display.
    pub fn start_fill_to(&mut self, target: Addr) {
        let (c0, r0, c1, r1) = self.current_range().bounds();
        self.fill_drag = Some(FillDrag {
            src: ((c0, r0), (c1, r1)),
            target,
        });
        self.pointer_up();
    }

    /// Visible column indices given the current scroll + body width.
    fn visible_cols(&self) -> Vec<u32> {
        let body = self.body_rect();
        let start = self
            .col_x
            .partition_point(|&x| x <= self.scroll_x)
            .saturating_sub(1)
            .min(self.n_cols as usize);
        let end = self
            .col_x
            .partition_point(|&x| x < self.scroll_x + body.width())
            .min(self.n_cols as usize);
        (start..end)
            .filter_map(|index| u32::try_from(index).ok())
            .collect()
    }
    fn visible_rows(&self) -> Vec<u32> {
        let body = self.body_rect();
        let start = self
            .row_y
            .partition_point(|&y| y <= self.scroll_y)
            .saturating_sub(1)
            .min(self.n_rows as usize);
        let end = self
            .row_y
            .partition_point(|&y| y < self.scroll_y + body.height())
            .min(self.n_rows as usize);
        (start..end)
            .filter_map(|index| u32::try_from(index).ok())
            .collect()
    }

    /// Map a screen point to a cell, or `None` if it's over a header/corner.
    fn cell_at(&self, p: Point) -> Option<Addr> {
        let body = self.body_rect();
        if !body.contains(p) {
            return None;
        }
        let cx = p.x - self.grid_x0() + self.scroll_x;
        let cy = p.y - self.grid_y0() + self.scroll_y;
        let mut x = 0.0;
        let mut col = None;
        for c in 0..self.n_cols {
            let w = self.col_w(c);
            if cx >= x && cx < x + w {
                col = Some(c);
                break;
            }
            x += w;
        }
        let mut y = 0.0;
        let mut row = None;
        for r in 0..self.n_rows {
            let h = self.row_h(r);
            if cy >= y && cy < y + h {
                row = Some(r);
                break;
            }
            y += h;
        }
        Some((col?, row?))
    }

    /// If `p` is near a column/row right/bottom edge in the header strip,
    /// return the resize target.
    fn resize_target_at(&self, p: Point) -> Option<ResizeTarget> {
        // Column edges live in the column-header strip.
        if p.y >= self.rect.y0 && p.y < self.grid_y0() && p.x >= self.grid_x0() {
            let mut x = self.grid_x0() - self.scroll_x;
            for c in 0..self.n_cols {
                x += self.col_w(c);
                if (p.x - x).abs() <= RESIZE_GRAB * self.text_scale {
                    return Some(ResizeTarget::Col(c));
                }
                if x > self.rect.x1 {
                    break;
                }
            }
        }
        // Row edges live in the row-header strip.
        if p.x >= self.rect.x0 && p.x < self.grid_x0() && p.y >= self.grid_y0() {
            let mut y = self.grid_y0() - self.scroll_y;
            for r in 0..self.n_rows {
                y += self.row_h(r);
                if (p.y - y).abs() <= RESIZE_GRAB * self.text_scale {
                    return Some(ResizeTarget::Row(r));
                }
                if y > self.rect.y1 {
                    break;
                }
            }
        }
        None
    }

    fn total_width(&self) -> f64 {
        self.col_x.last().copied().unwrap_or(0.0)
    }
    fn total_height(&self) -> f64 {
        self.row_y.last().copied().unwrap_or(0.0)
    }

    /// True once the grid has a usable layout rect (post `apply_layout`).
    fn has_layout(&self) -> bool {
        self.rect.width() > self.metrics.row_header_w()
            && self.rect.height() > self.metrics.col_header_h()
    }

    fn clamp_scroll(&mut self) {
        if !self.has_layout() {
            return;
        }
        let body = self.body_rect();
        let max_x = (self.total_width() - body.width()).max(0.0);
        let max_y = (self.total_height() - body.height()).max(0.0);
        self.scroll_x = self.scroll_x.clamp(0.0, max_x);
        self.scroll_y = self.scroll_y.clamp(0.0, max_y);
    }

    /// Scroll so the active cell is fully inside the body.
    fn ensure_visible(&mut self) {
        if !self.has_layout() {
            return;
        }
        let body = self.body_rect();
        let left = self.col_left(self.active.0);
        let right = left + self.col_w(self.active.0);
        let top = self.row_top(self.active.1);
        let bottom = top + self.row_h(self.active.1);
        if left - self.scroll_x < 0.0 {
            self.scroll_x = left;
        } else if right - self.scroll_x > body.width() {
            self.scroll_x = right - body.width();
        }
        if top - self.scroll_y < 0.0 {
            self.scroll_y = top;
        } else if bottom - self.scroll_y > body.height() {
            self.scroll_y = bottom - body.height();
        }
        self.clamp_scroll();
    }

    // --- editing ------------------------------------------------------------

    /// Begin editing the active cell. `seed` replaces the content (typed
    /// character) or, when `None`, loads the existing raw for in-place edit.
    pub fn begin_edit(&mut self, seed: Option<String>) {
        let text = match seed {
            Some(s) => s,
            None => self.raw.get(&self.active).cloned().unwrap_or_default(),
        };
        let caret = text.len();
        self.edit = Some(EditState {
            addr: self.active,
            core: EditCore::at(caret),
            text,
        });
        self.point_anchor = None;
        self.point_selecting = false;
        self.dirty = true;
    }

    fn commit_edit(&mut self, out: &mut Vec<GridCmd>) {
        if let Some(e) = self.edit.take() {
            out.push(GridCmd::SetCell {
                addr: e.addr,
                raw: e.text(),
            });
            out.push(GridCmd::Commit);
            self.point_anchor = None;
            self.point_selecting = false;
            self.dirty = true;
        }
    }

    fn cancel_edit(&mut self) {
        self.edit = None;
        self.point_anchor = None;
        self.point_selecting = false;
        self.dirty = true;
    }

    /// Whether an in-cell edit is in progress and its buffer is a formula
    /// (begins with `=`). Formula edits enter "point mode": clicking cells
    /// inserts their references instead of moving the selection.
    pub fn editing_formula(&self) -> bool {
        self.edit.as_ref().is_some_and(|e| e.text.starts_with('='))
    }

    /// The formula body (text after `=`) whose references should be
    /// highlighted this frame: the live edit buffer while a formula is being
    /// edited, otherwise the active cell's stored formula when it is merely
    /// selected. Returns `None` when neither is a formula, so plain cells stay
    /// uncluttered. Exposed so the app's formula bar can tint the same refs.
    pub fn highlight_formula_body(&self) -> Option<String> {
        if let Some(e) = &self.edit {
            return e
                .text()
                .strip_prefix('=')
                .map(std::string::ToString::to_string);
        }
        // Not editing: a single selected formula cell still shows its trace.
        let raw = self.raw.get(&self.active)?;
        raw.strip_prefix('=').map(std::string::ToString::to_string)
    }

    /// Whether a click can be absorbed as a reference insertion right now: a
    /// formula is being edited, and either a click-inserted reference is
    /// pending replacement or the caret sits right after an operator/opener.
    fn can_point(&self) -> bool {
        let Some(e) = &self.edit else { return false };
        if !e.text.starts_with('=') {
            return false;
        }
        if self.point_anchor.is_some() {
            return true;
        }
        let caret = e.caret();
        let prev = if caret == 0 {
            None
        } else {
            e.text.chars().nth(caret - 1)
        };
        matches!(
            prev,
            Some('=' | '+' | '-' | '*' | '/' | '^' | '(' | ',' | ':' | '&' | '<' | '>' | '%')
        )
    }

    /// Insert (or, when a click-inserted reference is pending, replace) a
    /// single-cell reference to `addr` at the caret, and arm a potential drag
    /// that would grow it into a range.
    fn begin_point(&mut self, addr: Addr) {
        let insert_at = match self.point_anchor {
            Some(a) => {
                if let Some(e) = &mut self.edit {
                    let byte = e
                        .text
                        .char_indices()
                        .nth(a)
                        .map_or(e.text.len(), |(byte, _)| byte);
                    e.text.truncate(byte);
                    e.core.set_caret(byte);
                }
                a
            }
            None => self.edit.as_ref().map_or(0, EditState::caret),
        };
        let s = addr_to_a1(addr.0, addr.1);
        if let Some(e) = &mut self.edit {
            let edit = e.core.insert(&s);
            e.apply_edit(&edit);
        }
        self.point_anchor = Some(insert_at);
        self.point_drag_start = addr;
        self.point_selecting = true;
        self.dirty = true;
    }

    /// Extend the pending point-mode reference into the range
    /// `point_drag_start:addr` as the pointer is dragged.
    fn point_drag_to(&mut self, addr: Addr) {
        let Some(anchor) = self.point_anchor else {
            return;
        };
        let start = self.point_drag_start;
        let text = if start == addr {
            addr_to_a1(addr.0, addr.1)
        } else {
            let tl = (start.0.min(addr.0), start.1.min(addr.1));
            let br = (start.0.max(addr.0), start.1.max(addr.1));
            format!("{}:{}", addr_to_a1(tl.0, tl.1), addr_to_a1(br.0, br.1))
        };
        if let Some(e) = &mut self.edit {
            let byte = e
                .text
                .char_indices()
                .nth(anchor)
                .map_or(e.text.len(), |(byte, _)| byte);
            e.text.truncate(byte);
            e.core.set_caret(byte);
            let edit = e.core.insert(&text);
            e.apply_edit(&edit);
        }
        self.dirty = true;
    }

    fn move_active(&mut self, dc: i32, dr: i32, extend: bool) {
        // Keyboard navigation collapses a discontiguous multi-selection back to
        // the live range (Shift+arrow extends it; a bare arrow resets it),
        // matching desktop spreadsheets.
        self.extra_ranges.clear();
        let nc = (self.active.0 as i32 + dc).clamp(0, self.n_cols as i32 - 1) as u32;
        let nr = (self.active.1 as i32 + dr).clamp(0, self.n_rows as i32 - 1) as u32;
        self.active = (nc, nr);
        if !extend {
            self.anchor = self.active;
        }
        self.ensure_visible();
        self.dirty = true;
    }

    // --- input handlers (called by the app) --------------------------------

    pub fn on_scroll(&mut self, dx: f64, dy: f64) {
        self.scroll_x += dx;
        self.scroll_y += dy;
        self.clamp_scroll();
        self.dirty = true;
    }

    /// Handle a left-press. `shift` extends the live range from its anchor;
    /// `ctrl` starts a new discontiguous range (Ctrl+click / Ctrl+drag),
    /// keeping the existing selection. Returns true if consumed by the grid.
    pub fn pointer_down(
        &mut self,
        p: Point,
        shift: bool,
        ctrl: bool,
        out: &mut Vec<GridCmd>,
    ) -> bool {
        self.last_cursor = p;
        if !self.rect.contains(p) {
            return false;
        }
        if self.try_start_scrollbar_drag(p) {
            return true;
        }
        if self.try_point_mode_click(p) {
            return true;
        }
        if self.try_start_resize(p) {
            return true;
        }
        if !ctrl && !shift && self.try_start_fill_drag(p, out) {
            return true;
        }
        if self.try_column_header_click(p, ctrl, shift, out) {
            return true;
        }
        if self.try_row_header_click(p, ctrl, shift, out) {
            return true;
        }
        self.try_body_click(p, ctrl, shift, out)
    }

    fn try_start_scrollbar_drag(&mut self, p: Point) -> bool {
        for axis in [ScrollAxis::Horizontal, ScrollAxis::Vertical] {
            let rail = match axis {
                ScrollAxis::Horizontal => self.horizontal_scrollbar_rect(),
                ScrollAxis::Vertical => self.vertical_scrollbar_rect(),
            };
            if !rail.contains(p) {
                continue;
            }
            let thumb = self.scrollbar_thumb(axis);
            let pointer = match axis {
                ScrollAxis::Horizontal => p.x,
                ScrollAxis::Vertical => p.y,
            };
            let thumb_start = match axis {
                ScrollAxis::Horizontal => thumb.x0,
                ScrollAxis::Vertical => thumb.y0,
            };
            let thumb_end = match axis {
                ScrollAxis::Horizontal => thumb.x1,
                ScrollAxis::Vertical => thumb.y1,
            };
            let origin = if pointer < thumb_start || pointer > thumb_end {
                let rail_start = match axis {
                    ScrollAxis::Horizontal => rail.x0,
                    ScrollAxis::Vertical => rail.y0,
                };
                let rail_len = match axis {
                    ScrollAxis::Horizontal => rail.width(),
                    ScrollAxis::Vertical => rail.height(),
                };
                let thumb_len = thumb_end - thumb_start;
                let max_scroll = match axis {
                    ScrollAxis::Horizontal => self.max_scroll_x(),
                    ScrollAxis::Vertical => self.max_scroll_y(),
                };
                let next = ((pointer - rail_start - thumb_len / 2.0)
                    / (rail_len - thumb_len).max(1.0)
                    * max_scroll)
                    .clamp(0.0, max_scroll);
                match axis {
                    ScrollAxis::Horizontal => self.scroll_x = next,
                    ScrollAxis::Vertical => self.scroll_y = next,
                }
                pointer
            } else {
                pointer
            };
            self.scroll_drag = Some(ScrollDrag {
                axis,
                pointer_origin: origin,
                scroll_origin: match axis {
                    ScrollAxis::Horizontal => self.scroll_x,
                    ScrollAxis::Vertical => self.scroll_y,
                },
            });
            self.dirty = true;
            return true;
        }
        false
    }

    /// Formula point mode: while editing a formula, a click on another cell
    /// inserts its reference (a drag inserts a range) rather than moving
    /// the selection or committing the edit. Only fires when the caret is
    /// ready for a reference (right after `=` / an operator, or replacing a
    /// previously click-inserted one); otherwise the click falls through to
    /// normal selection, which commits the edit.
    fn try_point_mode_click(&mut self, p: Point) -> bool {
        if self.editing_formula()
            && self.can_point()
            && let Some(addr) = self.cell_at(p)
        {
            let editing_here = self.edit.as_ref().map(|e| e.addr) == Some(addr);
            if !editing_here {
                self.begin_point(addr);
                return true;
            }
        }
        false
    }

    /// Start a resize drag if `p` is near a column/row header edge.
    fn try_start_resize(&mut self, p: Point) -> bool {
        if let Some(target) = self.resize_target_at(p) {
            let origin_size = match target {
                ResizeTarget::Col(c) => self.col_w(c),
                ResizeTarget::Row(r) => self.row_h(r),
            };
            let origin_px = match target {
                ResizeTarget::Col(_) => p.x,
                ResizeTarget::Row(_) => p.y,
            };
            self.resize = Some(ResizeDrag {
                target,
                origin_px,
                origin_size,
            });
            return true;
        }
        false
    }

    /// Fill handle: grab the selection's bottom-right corner and drag to
    /// extend the pattern. A plain press only (the caller gates on
    /// Ctrl/Shift, which keep their normal selection meaning). Checked
    /// before the body-click so a grab near the corner starts a fill rather
    /// than a new selection.
    fn try_start_fill_drag(&mut self, p: Point, out: &mut Vec<GridCmd>) -> bool {
        if let Some(hr) = self.fill_handle_rect() {
            let grab = hr.inflate(FILL_HANDLE_GRAB, FILL_HANDLE_GRAB);
            if grab.contains(p) {
                self.commit_edit(out);
                let (c0, r0, c1, r1) = self.current_range().bounds();
                self.fill_drag = Some(FillDrag {
                    src: ((c0, r0), (c1, r1)),
                    target: (c1, r1),
                });
                self.dirty = true;
                return true;
            }
        }
        false
    }

    /// Column header click → select the whole column (Ctrl adds it, Shift
    /// extends the column span from the current anchor).
    fn try_column_header_click(
        &mut self,
        p: Point,
        ctrl: bool,
        shift: bool,
        out: &mut Vec<GridCmd>,
    ) -> bool {
        if p.y >= self.rect.y0
            && p.y < self.grid_y0()
            && p.x >= self.grid_x0()
            && let Some((c, _)) = self.cell_at(Point::new(p.x, self.grid_y0() + 1.0))
        {
            self.commit_edit(out);
            let bottom = self.n_rows.saturating_sub(1);
            if ctrl {
                self.extra_ranges.push(self.current_range());
                self.anchor = (c, bottom);
            } else if shift {
                self.anchor = (self.anchor.0, bottom);
            } else {
                self.extra_ranges.clear();
                self.anchor = (c, bottom);
            }
            self.active = (c, 0);
            self.selecting = true;
            self.dirty = true;
            return true;
        }
        false
    }

    /// Row header click → select whole row (Ctrl adds, Shift extends).
    fn try_row_header_click(
        &mut self,
        p: Point,
        ctrl: bool,
        shift: bool,
        out: &mut Vec<GridCmd>,
    ) -> bool {
        if p.x >= self.rect.x0
            && p.x < self.grid_x0()
            && p.y >= self.grid_y0()
            && let Some((_, r)) = self.cell_at(Point::new(self.grid_x0() + 1.0, p.y))
        {
            self.commit_edit(out);
            let right = self.n_cols.saturating_sub(1);
            if ctrl {
                self.extra_ranges.push(self.current_range());
                self.anchor = (right, r);
            } else if shift {
                self.anchor = (right, self.anchor.1);
            } else {
                self.extra_ranges.clear();
                self.anchor = (right, r);
            }
            self.active = (0, r);
            self.selecting = true;
            self.dirty = true;
            return true;
        }
        false
    }

    /// Body click → select / extend / add.
    fn try_body_click(
        &mut self,
        p: Point,
        ctrl: bool,
        shift: bool,
        out: &mut Vec<GridCmd>,
    ) -> bool {
        if let Some(addr) = self.cell_at(p) {
            self.commit_edit(out);
            if ctrl {
                // Commit the live range and start a fresh one at the clicked
                // cell; a subsequent drag grows it into a new rectangle.
                self.extra_ranges.push(self.current_range());
                self.anchor = addr;
                self.active = addr;
            } else if shift {
                // Extend the live range; keep its anchor and any other ranges.
                self.active = addr;
            } else {
                self.extra_ranges.clear();
                self.anchor = addr;
                self.active = addr;
            }
            self.selecting = true;
            self.dirty = true;
            return true;
        }
        false
    }

    pub fn pointer_move(&mut self, p: Point) -> bool {
        self.last_cursor = p;
        if let Some(drag) = &self.scroll_drag {
            let rail = match drag.axis {
                ScrollAxis::Horizontal => self.horizontal_scrollbar_rect(),
                ScrollAxis::Vertical => self.vertical_scrollbar_rect(),
            };
            let thumb = self.scrollbar_thumb(drag.axis);
            let rail_len = match drag.axis {
                ScrollAxis::Horizontal => rail.width(),
                ScrollAxis::Vertical => rail.height(),
            };
            let thumb_len = match drag.axis {
                ScrollAxis::Horizontal => thumb.width(),
                ScrollAxis::Vertical => thumb.height(),
            };
            let pointer = match drag.axis {
                ScrollAxis::Horizontal => p.x,
                ScrollAxis::Vertical => p.y,
            };
            let max_scroll = match drag.axis {
                ScrollAxis::Horizontal => self.max_scroll_x(),
                ScrollAxis::Vertical => self.max_scroll_y(),
            };
            let next = drag.scroll_origin
                + (pointer - drag.pointer_origin) * max_scroll / (rail_len - thumb_len).max(1.0);
            match drag.axis {
                ScrollAxis::Horizontal => self.scroll_x = next,
                ScrollAxis::Vertical => self.scroll_y = next,
            }
            self.clamp_scroll();
            self.dirty = true;
            return true;
        }
        // A held fill-handle drag tracks the pointer; the projected region is
        // recomputed (and previewed) each frame.
        if self.fill_drag.is_some() {
            if let Some(addr) = self.cell_at(p)
                && let Some(fd) = &mut self.fill_drag
                && addr != fd.target
            {
                fd.target = addr;
                self.dirty = true;
            }
            return true;
        }
        // A held point-mode drag grows the pending reference into a range.
        if self.point_selecting {
            if let Some(addr) = self.cell_at(p) {
                self.point_drag_to(addr);
            }
            return true;
        }
        if let Some(drag) = &self.resize {
            match drag.target {
                ResizeTarget::Col(c) => {
                    let w = (drag.origin_size + (p.x - drag.origin_px))
                        .max(MIN_COL_W * self.text_scale);
                    self.col_widths.insert(c, w / self.text_scale);
                }
                ResizeTarget::Row(r) => {
                    let h = (drag.origin_size + (p.y - drag.origin_px))
                        .max(MIN_ROW_H * self.text_scale);
                    self.row_heights.insert(r, h / self.text_scale);
                }
            }
            self.rebuild_geom();
            self.dirty = true;
            return true;
        }
        if self.selecting {
            if let Some(addr) = self.cell_at(p)
                && addr != self.active
            {
                self.active = addr;
                self.dirty = true;
            }
            return true;
        }
        false
    }

    pub fn pointer_up(&mut self) {
        self.resize = None;
        self.scroll_drag = None;
        self.selecting = false;
        // Keep `point_anchor` so the next click replaces the just-inserted
        // reference (Excel keeps a click-inserted ref "hot" until you type);
        // only the drag itself ends here.
        self.point_selecting = false;
        // A fill-handle drag commits its projected region for the app to fill,
        // then extends the visible selection over it.
        if let Some(fd) = self.fill_drag.take()
            && let Some((stl, sbr, dtl, dbr)) = Self::fill_region(fd.src, fd.target)
        {
            self.pending_fill = Some((stl, sbr, dtl, dbr));
            self.anchor = stl;
            self.active = dbr;
            self.dirty = true;
        }
    }

    pub fn double_click(&mut self, p: Point, out: &mut Vec<GridCmd>) -> bool {
        let _ = out;
        if let Some(addr) = self.cell_at(p) {
            self.extra_ranges.clear();
            self.active = addr;
            self.anchor = addr;
            self.begin_edit(None);
            return true;
        }
        false
    }

    /// Handle a keyboard event. Returns true if consumed.
    pub fn key(&mut self, event: &WindowEvent, mods: &Modifiers, out: &mut Vec<GridCmd>) -> bool {
        let WindowEvent::KeyboardInput { event: ke, .. } = event else {
            return false;
        };
        if !ke.state.is_pressed() {
            return false;
        }
        let shift = mods.state().shift_key();
        let ctrl = mods.state().control_key() || mods.state().super_key();

        // Editing mode owns most keys.
        if self.edit.is_some() {
            return self.key_editing(ke, out, shift);
        }

        match &ke.logical_key {
            Key::Named(NamedKey::ArrowUp) => {
                self.move_active(0, -1, shift);
                true
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.move_active(0, 1, shift);
                true
            }
            Key::Named(NamedKey::ArrowLeft) => {
                self.move_active(-1, 0, shift);
                true
            }
            Key::Named(NamedKey::ArrowRight) => {
                self.move_active(1, 0, shift);
                true
            }
            Key::Named(NamedKey::Tab) => {
                self.move_active(if shift { -1 } else { 1 }, 0, false);
                true
            }
            Key::Named(NamedKey::Enter) => {
                self.move_active(0, if shift { -1 } else { 1 }, false);
                true
            }
            Key::Named(NamedKey::F2) => {
                self.begin_edit(None);
                true
            }
            Key::Named(NamedKey::Home) => {
                self.extra_ranges.clear();
                self.active.0 = 0;
                if !shift {
                    self.anchor = self.active;
                }
                self.ensure_visible();
                self.dirty = true;
                true
            }
            Key::Named(NamedKey::PageDown) => {
                self.move_active(0, 12, shift);
                true
            }
            Key::Named(NamedKey::PageUp) => {
                self.move_active(0, -12, shift);
                true
            }
            Key::Named(NamedKey::Delete | NamedKey::Backspace) => {
                for addr in self.selected_cells() {
                    out.push(GridCmd::SetCell {
                        addr,
                        raw: String::new(),
                    });
                }
                out.push(GridCmd::Commit);
                self.dirty = true;
                true
            }
            Key::Character(s) if !ctrl => {
                // Begin editing, replacing content with the typed char.
                self.begin_edit(Some(s.to_string()));
                true
            }
            _ => false,
        }
    }

    fn key_editing(
        &mut self,
        ke: &winit::event::KeyEvent,
        out: &mut Vec<GridCmd>,
        shift: bool,
    ) -> bool {
        match &ke.logical_key {
            Key::Named(NamedKey::Enter) => {
                self.commit_edit(out);
                self.move_active(0, 1, false);
                true
            }
            Key::Named(NamedKey::Tab) => {
                self.commit_edit(out);
                self.move_active(1, 0, false);
                true
            }
            Key::Named(NamedKey::Escape) => {
                self.cancel_edit();
                true
            }
            Key::Named(NamedKey::Backspace) => {
                if let Some(e) = &mut self.edit {
                    let text = e.text.clone();
                    let edit = e.core.delete_backward(&text, false);
                    e.apply_edit(&edit);
                }
                self.point_anchor = None;
                self.dirty = true;
                true
            }
            Key::Named(NamedKey::Delete) => {
                if let Some(e) = &mut self.edit {
                    let text = e.text.clone();
                    let edit = e.core.delete_forward(&text, false);
                    e.apply_edit(&edit);
                }
                self.point_anchor = None;
                self.dirty = true;
                true
            }
            Key::Named(NamedKey::ArrowLeft) => {
                self.edit_move_caret(CaretMove::Left, shift);
                true
            }
            Key::Named(NamedKey::ArrowRight) => {
                self.edit_move_caret(CaretMove::Right, shift);
                true
            }
            Key::Named(NamedKey::Home) => {
                self.edit_move_caret(CaretMove::Home, shift);
                true
            }
            Key::Named(NamedKey::End) => {
                self.edit_move_caret(CaretMove::End, shift);
                true
            }
            Key::Named(NamedKey::Space) => {
                self.insert_text(" ");
                true
            }
            Key::Character(s) => {
                self.insert_text(s);
                true
            }
            _ => {
                // Fall back to the platform-resolved text (handles symbols).
                if let Some(t) = &ke.text {
                    self.insert_text(t);
                    true
                } else {
                    false
                }
            }
        }
    }

    fn insert_text(&mut self, s: &str) {
        if let Some(e) = &mut self.edit {
            let edit = e.core.insert(s);
            e.apply_edit(&edit);
            // Typing commits any click-inserted reference into ordinary text,
            // so the next click appends a fresh reference instead of replacing.
            self.point_anchor = None;
            self.dirty = true;
        }
    }

    /// Remove the selected characters (if any), collapsing the caret to the
    /// selection start. Returns whether anything was removed.
    fn delete_selection(&mut self) -> bool {
        let Some(e) = &mut self.edit else {
            return false;
        };
        let text = e.text.clone();
        let edit = e.core.delete_backward(&text, false);
        if edit.is_noop() {
            return false;
        }
        e.apply_edit(&edit);
        self.point_anchor = None;
        self.dirty = true;
        true
    }

    /// The caret's char index within the current edit buffer, if editing.
    pub fn edit_caret(&self) -> Option<usize> {
        self.edit.as_ref().map(EditState::caret)
    }

    /// The ordered `(start, end)` selection within the edit buffer, if any.
    pub fn edit_selection(&self) -> Option<(usize, usize)> {
        self.edit.as_ref().and_then(EditState::selection)
    }

    /// Move the edit caret to char index `idx`. When `extend` is set, the
    /// selection grows from its existing anchor (or the old caret); otherwise
    /// the selection is cleared. Used by the formula bar for click, shift-click
    /// and drag-select. A no-op when not editing.
    pub fn set_edit_caret(&mut self, idx: usize, extend: bool) {
        if let Some(e) = &mut self.edit {
            e.set_caret(idx.min(e.text.chars().count()), extend);
            self.point_anchor = None;
            self.dirty = true;
        }
    }

    /// Select the whole edit buffer (Ctrl+A inside the formula bar / cell).
    pub fn select_all_edit(&mut self) {
        if let Some(e) = &mut self.edit {
            let text = e.text.clone();
            e.core.select_all(&text);
            self.point_anchor = None;
            self.dirty = true;
        }
    }

    /// The currently selected text (for Ctrl+C in the field), or `None` when
    /// there is no selection.
    pub fn edit_selected_text(&self) -> Option<String> {
        let e = self.edit.as_ref()?;
        let range = e.core.range();
        (!range.is_empty())
            .then(|| e.text.get(range).map(str::to_owned))
            .flatten()
    }

    /// Cut: return the selected text and remove it from the buffer.
    pub fn edit_cut_text(&mut self) -> Option<String> {
        let text = self.edit_selected_text()?;
        self.delete_selection();
        Some(text)
    }

    /// Paste `s` into the field, replacing any selection. Newlines collapse to
    /// spaces so the single-line formula bar stays on one line.
    pub fn edit_paste_text(&mut self, s: &str) {
        if self.edit.is_none() {
            return;
        }
        let flat: String = s.replace(['\n', '\r', '\t'], " ");
        self.insert_text(&flat);
    }

    /// Move the caret for an arrow/Home/End key while editing. `shift` extends
    /// the selection; without it, an existing selection collapses to the edge
    /// in the motion's direction.
    fn edit_move_caret(&mut self, mv: CaretMove, shift: bool) {
        if let Some(e) = &mut self.edit {
            let motion = match mv {
                CaretMove::Left => Motion::Left,
                CaretMove::Right => Motion::Right,
                CaretMove::Home => Motion::LineStart,
                CaretMove::End => Motion::LineEnd,
            };
            e.core.motion(&e.text, motion, shift);
            self.point_anchor = None;
            self.dirty = true;
        }
    }

    /// The app calls this when the user types into the formula bar / clicks
    /// "fx" to start an edit seeded with the current cell's content.
    pub fn begin_edit_in_place(&mut self) {
        if self.edit.is_none() {
            self.begin_edit(None);
        }
    }

    /// Commit any pending edit (e.g. before switching sheets).
    pub fn flush_edit(&mut self, out: &mut Vec<GridCmd>) {
        self.commit_edit(out);
    }

    /// Shots/test hook: insert (or replace) a click-inserted reference to
    /// `addr` while editing a formula — the point-mode click, driven by
    /// address so it works before the grid has a laid-out geometry.
    pub fn point_click_addr(&mut self, addr: Addr) {
        self.begin_point(addr);
        self.point_selecting = false;
    }

    /// Shots/test hook: type literal text into the active edit buffer.
    pub fn insert_text_public(&mut self, s: &str) {
        self.insert_text(s);
    }

    // --- rendering ----------------------------------------------------------

    /// Fit each column to the width of its widest rendered cell so long labels
    /// (e.g. a row title in column A) aren't clipped when the neighbouring cell
    /// is occupied — matching the spreadsheet convention of auto-sizing a
    /// column to its content. A column the user has manually resized keeps its
    /// width; the rest grow to fit, clamped to `[DEFAULT_COL_W, MAX_AUTOFIT_W]`
    /// so they never shrink below the default nor run away for an extreme
    /// value. Measured with the same font size the cells are drawn at.
    fn autofit_columns(&mut self, tr: &mut TextRenderer, size: f32) {
        self.auto_col_w.clear();
        let mut widest: HashMap<u32, f64> = HashMap::new();
        for (&(c, r), val) in &self.values {
            // Skip empties and columns the user pinned to a manual width.
            if val.is_empty() || self.col_widths.contains_key(&c) {
                continue;
            }
            // Fit to what's actually on screen: the raw formula in formula
            // view, otherwise the formatted value.
            let text = if self.show_formulas {
                match self.raw.get(&(c, r)) {
                    Some(s) if !s.is_empty() => s.clone(),
                    _ => continue,
                }
            } else {
                let fmt = self.fmt.get(&(c, r)).copied().unwrap_or_default();
                format_value(val, fmt)
            };
            if text.is_empty() {
                continue;
            }
            let (tw, _) = tr.measure(&text, size, None);
            let want = f64::from(tw) + CELL_PAD_X * self.text_scale * 2.0;
            let entry = widest.entry(c).or_insert(0.0);
            if want > *entry {
                *entry = want;
            }
        }
        for (c, want) in widest {
            let fitted = want.clamp(
                DEFAULT_COL_W * self.text_scale,
                MAX_AUTOFIT_W * self.text_scale,
            );
            // Only record a widening; narrower columns fall back to the default.
            if fitted > DEFAULT_COL_W * self.text_scale {
                self.auto_col_w.insert(c, fitted);
            }
        }
    }

    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        // Refit columns to their content before laying anything out, so long
        // labels aren't truncated by an occupied neighbour.
        if self.needs_autofit {
            let cell_size = self.metrics.cell_size;
            self.autofit_columns(tr, cell_size);
            self.rebuild_geom();
            self.needs_autofit = false;
        }
        let body = self.body_rect();
        let cols = self.visible_cols();
        let rows = self.visible_rows();

        // Body background.
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &self.rect,
        );
        self.draw_selection_fill(scene, transform, theme, body);

        // --- cells + text (clipped to the body) ---
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &body);
        self.draw_cells(scene, transform, tr, theme, &cols, &rows);
        self.draw_grid_lines(scene, transform, theme, &cols, &rows, body);
        self.draw_active_highlight(scene, transform, theme, body);
        self.draw_fill_handle(scene, transform, theme, body);
        self.draw_reference_highlights(scene, transform, theme, body);
        scene.pop_layer();

        // --- headers (painted on top, never clipped by the body) ---
        self.draw_headers(scene, transform, tr, theme, &cols, &rows);

        // Visible rails make both axes discoverable, and their 16px targets
        // remain usable with a mouse, touchpad-generated pointer drag, or a
        // high-contrast palette. The thumb position is derived solely from
        // the same scroll offsets the viewport uses.
        self.draw_scrollbars(scene, transform, theme);

        // --- in-cell editor overlay ---
        if self.edit.is_some() {
            self.draw_editor(scene, transform, tr, theme);
        }
    }

    fn draw_scrollbars(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let rail = theme.tokens.colors.surfaces.raised;
        let thumb = theme.tokens.colors.accent[7];
        for axis in [ScrollAxis::Horizontal, ScrollAxis::Vertical] {
            let track = match axis {
                ScrollAxis::Horizontal => self.horizontal_scrollbar_rect(),
                ScrollAxis::Vertical => self.vertical_scrollbar_rect(),
            };
            scene.fill(Fill::NonZero, transform, rail, None, &track);
            // The thumb's edge is derived against the thumb itself, so it
            // stays a crisp inset hairline on every accent (HIG §9.5).
            ui::surface::card(scene, transform, self.scrollbar_thumb(axis), 0.0, thumb);
        }
        let body = self.body_rect();
        let corner = Rect::new(body.x1, body.y1, self.rect.x1, self.rect.y1);
        scene.fill(Fill::NonZero, transform, rail, None, &corner);
    }

    /// Selection ranges (translucent accent fill) under the cells. Every
    /// discontiguous range paints; overlaps just deepen slightly. Iterate
    /// without allocating a ranges vector on the per-frame draw path.
    /// Read the accent from the theme so the selection wash tracks the OS
    /// palette / high-contrast, matching the name-box + tab accents nearby.
    fn draw_selection_fill(&self, scene: &mut Scene, transform: Affine, theme: &Theme, body: Rect) {
        let fill = theme.tokens.colors.accent[5].with_alpha_factor(0.15);
        for range in self
            .extra_ranges
            .iter()
            .copied()
            .chain(std::iter::once(self.current_range()))
        {
            let sr = self.range_screen_rect(range);
            if let Some(cr) = intersect(sr, body) {
                scene.fill(Fill::NonZero, transform, fill, None, &cr);
            }
        }
    }

    /// Paint each visible cell's value/formula text.
    fn draw_cells(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        cols: &[u32],
        rows: &[u32],
    ) {
        let colors = &theme.tokens.colors;
        let mut ctx = PaintCtx::new(scene, transform, tr, theme);
        for &r in rows {
            for &c in cols {
                let cell = self.cell_screen_rect(c, r);
                // Skip while editing this exact cell — the editor overlay
                // paints it instead.
                if let Some(e) = &self.edit
                    && e.addr == (c, r)
                {
                    continue;
                }
                // Formula view: show the raw input verbatim (formulas as
                // typed), left-aligned in the accent colour so it reads as
                // "source" rather than a result.
                if self.show_formulas
                    && let Some(raw) = self.raw.get(&(c, r))
                    && !raw.is_empty()
                {
                    let color = if raw.starts_with('=') {
                        colors.accent[5]
                    } else {
                        colors.text_primary
                    };
                    self.draw_cell_text(&mut ctx, raw, color, cell, false, TextWeight::Regular);
                    continue;
                }
                if let Some(val) = self.values.get(&(c, r)) {
                    if val.is_empty() {
                        continue;
                    }
                    let fmt = self.fmt.get(&(c, r)).copied().unwrap_or_default();
                    let text = format_value(val, fmt);
                    if text.is_empty() {
                        continue;
                    }
                    let color = if val.is_error() {
                        colors.semantic.danger
                    } else {
                        colors.text_primary
                    };
                    let right_align = val.is_numeric() && fmt != CellFormat::Text;
                    let weight = if self.bold.contains(&(c, r)) {
                        TextWeight::Bold
                    } else {
                        TextWeight::Regular
                    };
                    self.draw_cell_text(&mut ctx, &text, color, cell, right_align, weight);
                }
            }
        }
    }

    /// Grid line strokes for the visible rows/columns, plus the trailing
    /// right/bottom edges when they fall within `body`.
    fn draw_grid_lines(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        cols: &[u32],
        rows: &[u32],
        body: Rect,
    ) {
        let line_color = theme.tokens.colors.border_standard.with_alpha_factor(0.5);
        let stroke = Stroke::new(1.0);
        // verticals
        for &c in cols {
            let x = (self.grid_x0() + self.col_left(c) - self.scroll_x).round() + 0.5;
            scene.stroke(
                &stroke,
                transform,
                line_color,
                None,
                &Line::new((x, body.y0), (x, body.y1)),
            );
        }
        // last right edge
        if let Some(&lastc) = cols.last() {
            let x = (self.grid_x0() + self.col_left(lastc) + self.col_w(lastc) - self.scroll_x)
                .round()
                + 0.5;
            if x < body.x1 {
                scene.stroke(
                    &stroke,
                    transform,
                    line_color,
                    None,
                    &Line::new((x, body.y0), (x, body.y1)),
                );
            }
        }
        for &r in rows {
            let y = (self.grid_y0() + self.row_top(r) - self.scroll_y).round() + 0.5;
            scene.stroke(
                &stroke,
                transform,
                line_color,
                None,
                &Line::new((body.x0, y), (body.x1, y)),
            );
        }
        if let Some(&lastr) = rows.last() {
            let y = (self.grid_y0() + self.row_top(lastr) + self.row_h(lastr) - self.scroll_y)
                .round()
                + 0.5;
            if y < body.y1 {
                scene.stroke(
                    &stroke,
                    transform,
                    line_color,
                    None,
                    &Line::new((body.x0, y), (body.x1, y)),
                );
            }
        }
    }

    /// Active-cell highlight border.
    fn draw_active_highlight(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        body: Rect,
    ) {
        if let Some(ar) = intersect(self.cell_screen_rect(self.active.0, self.active.1), body) {
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                theme.tokens.colors.accent[5],
                None,
                &ar,
            );
        }
    }

    /// Fill handle + drag preview. While dragging, a dashed outline shows
    /// the region the pattern will extend into; at rest, a small solid
    /// square sits on the selection's bottom-right corner (Excel's handle).
    fn draw_fill_handle(&self, scene: &mut Scene, transform: Affine, theme: &Theme, body: Rect) {
        let colors = &theme.tokens.colors;
        if let Some(fd) = self.fill_drag {
            if let Some((_, _, dtl, dbr)) = Self::fill_region(fd.src, fd.target) {
                let tl = self.cell_screen_rect(dtl.0, dtl.1);
                let br = self.cell_screen_rect(dbr.0, dbr.1);
                if let Some(cr) = intersect(Rect::new(tl.x0, tl.y0, br.x1, br.y1), body) {
                    let dashed = Stroke::new(1.5).with_dashes(0.0, [4.0, 3.0]);
                    scene.stroke(&dashed, transform, colors.accent[5], None, &cr);
                }
            }
        } else if let Some(hr) = self.fill_handle_rect()
            && let Some(cr) = intersect(hr, body)
        {
            // A thin base-coloured ring keeps the accent square legible on
            // any cell fill underneath.
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                colors.surfaces.base,
                None,
                &hr,
            );
            scene.fill(Fill::NonZero, transform, colors.accent[5], None, &cr);
        }
    }

    /// Referenced ranges of the formula being edited — coloured outlines
    /// matching the tinted references in the formula text, so it's clear
    /// which cells feed the result (and, in point mode, which range a click
    /// just inserted).
    fn draw_reference_highlights(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        body: Rect,
    ) {
        if let Some(body_text) = self.highlight_formula_body() {
            let spans = crate::formula::references(&body_text);
            let slots = crate::formula::ref_color_slots(&spans);
            for (span, slot) in spans.iter().zip(&slots) {
                let color = reference_color(theme, *slot);
                let tl = self.cell_screen_rect(span.tl.0, span.tl.1);
                let br = self.cell_screen_rect(span.br.0, span.br.1);
                let rr = Rect::new(tl.x0, tl.y0, br.x1, br.y1);
                if let Some(cr) = intersect(rr, body) {
                    scene.fill(
                        Fill::NonZero,
                        transform,
                        color.with_alpha_factor(0.12),
                        None,
                        &cr,
                    );
                    scene.stroke(&Stroke::new(1.5), transform, color, None, &cr);
                }
            }
        }
    }

    /// Absolute screen rect spanning one selection range (may lie off-body;
    /// the caller clips it).
    fn range_screen_rect(&self, range: SelRange) -> Rect {
        let (c0, r0, c1, r1) = range.bounds();
        let tl = self.cell_screen_rect(c0, r0);
        let br = self.cell_screen_rect(c1, r1);
        Rect::new(tl.x0, tl.y0, br.x1, br.y1)
    }

    fn draw_cell_text(
        &self,
        ctx: &mut PaintCtx<'_>,
        text: &str,
        color: Color,
        cell: Rect,
        right_align: bool,
        weight: TextWeight,
    ) {
        let PaintCtx {
            scene,
            transform,
            text: tr,
            ..
        } = ctx.reborrow();
        let size = self.metrics.cell_size;
        let line_h = self.metrics.cell_line_h;
        let inner = Rect::new(cell.x0 + 1.0, cell.y0, cell.x1 - 1.0, cell.y1);
        if inner.width() <= 2.0 {
            return;
        }
        let (tw, _) = tr.measure_weighted(text, size, None, weight);
        // Centre the measured line box, not the nominal font size, so a cell
        // and its row header share one baseline at every text scale.
        let ty = cell.y0 + (cell.height() - line_h) / 2.0;
        let tx = if right_align {
            cell.x1 - CELL_PAD_X - f64::from(tw)
        } else {
            cell.x0 + CELL_PAD_X * self.text_scale
        };
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &inner);
        tr.draw_text_weighted(
            scene,
            text,
            size,
            color,
            transform * Affine::translate((tx, ty)),
            None,
            weight,
        );
        scene.pop_layer();
    }

    fn draw_headers(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        cols: &[u32],
        rows: &[u32],
    ) {
        self.draw_col_headers(scene, transform, tr, theme, cols);
        self.draw_row_headers(scene, transform, tr, theme, rows);

        // Corner box.
        let colors = &theme.tokens.colors;
        let corner = Rect::new(self.rect.x0, self.rect.y0, self.grid_x0(), self.grid_y0());
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.raised,
            None,
            &corner,
        );
    }

    /// Column header strip: letters, selection tint, bottom border.
    fn draw_col_headers(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        cols: &[u32],
    ) {
        let colors = &theme.tokens.colors;
        let size = self.metrics.header_size;
        let line_h = self.metrics.header_line_h;

        let col_strip = Rect::new(self.rect.x0, self.rect.y0, self.rect.x1, self.grid_y0());
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.raised,
            None,
            &col_strip,
        );
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            transform,
            &col_strip,
        );
        for &c in cols {
            let cell = self.cell_screen_rect(c, 0);
            let hx = cell.x0;
            let hrect = Rect::new(hx, self.rect.y0, hx + self.col_w(c), self.grid_y0());
            let selected = self.col_in_selection(c);
            if selected {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colors.accent[5].with_alpha_factor(0.30),
                    None,
                    &hrect,
                );
            }
            let label = crate::model::col_to_letters(c);
            let (lw, _) = tr.measure(&label, size, None);
            let tx = hrect.center().x - f64::from(lw) / 2.0;
            let ty = hrect.y0 + (hrect.height() - line_h) / 2.0;
            let color = if selected {
                colors.text_primary
            } else {
                colors.text_dim
            };
            tr.draw_text(
                scene,
                &label,
                size,
                color,
                transform * Affine::translate((tx, ty)),
                None,
            );
        }
        scene.pop_layer();
        // Bottom edge of the column strip: the strip's own derived edge on
        // its last pixel row — `border_standard` is within a shade of the
        // dark raised strip and vanished there.
        scene.fill(
            Fill::NonZero,
            transform,
            ui::surface::edge_on(colors.surfaces.raised),
            None,
            &Rect::new(
                self.rect.x0,
                self.grid_y0() - 1.0,
                self.rect.x1,
                self.grid_y0(),
            ),
        );
    }

    /// Row header strip: numbers, selection tint, right border.
    fn draw_row_headers(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rows: &[u32],
    ) {
        let colors = &theme.tokens.colors;
        let size = self.metrics.header_size;
        let line_h = self.metrics.header_line_h;

        let row_strip = Rect::new(self.rect.x0, self.grid_y0(), self.grid_x0(), self.rect.y1);
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.raised,
            None,
            &row_strip,
        );
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            transform,
            &row_strip,
        );
        for &r in rows {
            let cell = self.cell_screen_rect(0, r);
            let hy = cell.y0;
            let hrect = Rect::new(self.rect.x0, hy, self.grid_x0(), hy + self.row_h(r));
            let selected = self.row_in_selection(r);
            if selected {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colors.accent[5].with_alpha_factor(0.30),
                    None,
                    &hrect,
                );
            }
            let label = format!("{}", r + 1);
            let (lw, _) = tr.measure(&label, size, None);
            let tx = hrect.center().x - f64::from(lw) / 2.0;
            let ty = hy + (self.row_h(r) - line_h) / 2.0;
            let color = if selected {
                colors.text_primary
            } else {
                colors.text_dim
            };
            tr.draw_text(
                scene,
                &label,
                size,
                color,
                transform * Affine::translate((tx, ty)),
                None,
            );
        }
        scene.pop_layer();
        // Right edge of the row strip: its derived edge on its last column.
        scene.fill(
            Fill::NonZero,
            transform,
            ui::surface::edge_on(colors.surfaces.raised),
            None,
            &Rect::new(
                self.grid_x0() - 1.0,
                self.rect.y0,
                self.grid_x0(),
                self.rect.y1,
            ),
        );
    }

    /// Is a cell non-empty in the current data? Used to decide how far the
    /// in-cell editor may overflow before a neighbour blocks it.
    fn cell_is_occupied(&self, addr: Addr) -> bool {
        match self.values.get(&addr) {
            Some(v) => !v.is_empty(),
            None => self.raw.get(&addr).is_some_and(|s| !s.is_empty()),
        }
    }

    /// The width the editor wants to display its text comfortably.
    fn editor_desired_width(&self, text: &str, tr: &mut TextRenderer, size: f32) -> f64 {
        let (tw, _) = tr.measure(text, size, None);
        // Text width + padding on both sides + caret room, never narrower
        // than the cell itself.
        (f64::from(tw) + CELL_PAD_X * self.text_scale * 2.0 + 8.0 * self.text_scale)
            .max(self.col_w(0))
    }

    /// Compute how far the floating editor for cell `addr` may extend to the
    /// right, in absolute screen x. It grows across **empty** neighbours
    /// until it has `desired` width, hits an occupied cell, runs out of
    /// columns, or reaches `body_right`. This is the pure geometry behind
    /// the "overflow into empty cells, clip at a non-empty neighbour" rule.
    fn editor_right_extent(&self, addr: Addr, desired: f64, body_right: f64) -> f64 {
        let (ec, er) = addr;
        let cell = self.cell_screen_rect(ec, er);
        let mut right = cell.x1;
        let mut next_col = ec + 1;
        while right - cell.x0 < desired && next_col < self.n_cols {
            if self.cell_is_occupied((next_col, er)) {
                break;
            }
            right += self.col_w(next_col);
            next_col += 1;
        }
        // Never overrun the visible body, but always show at least the cell.
        right.min(body_right).max(cell.x1.min(body_right))
    }

    /// Like Excel: the active edit is a floating box aligned to the cell
    /// that grows rightward into **empty** neighbours only, stopping at the
    /// first occupied cell or the grid's right edge. It is always clipped to
    /// the grid body so it never paints over the headers.
    fn draw_editor(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(e) = &self.edit else { return };
        let colors = &theme.tokens.colors;
        let (ec, er) = e.addr;
        let cell = self.cell_screen_rect(ec, er);
        let body = self.body_rect();

        // Don't paint an editor that scrolled out from under the body.
        if cell.y1 <= body.y0 || cell.x1 <= body.x0 || cell.x0 >= body.x1 || cell.y0 >= body.y1 {
            return;
        }

        let size = self.metrics.cell_size;
        let text = e.text();
        let desired = self.editor_desired_width(&text, tr, size);

        // Grow rightward across empty neighbours until we have enough room or
        // we hit an occupied cell / the last column / the grid edge.
        let right = self.editor_right_extent((ec, er), desired, body.x1);
        let edit_rect = Rect::new(cell.x0.max(body.x0), cell.y0, right, cell.y1);

        // Clip everything to the body so the floating editor stays inside the
        // grid even when it overflows past where data ends.
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &body);

        // Solid backing so underlying empty cells/gridlines don't bleed
        // through, then a crisp accent border — the "clean Excel" look.
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.base,
            None,
            &edit_rect,
        );
        scene.stroke(
            &Stroke::new(2.0),
            transform,
            colors.accent[5],
            None,
            &edit_rect,
        );

        // Text + caret, clipped to the editor box. A formula draws each cell
        // reference in its own colour; anything else draws in the body colour.
        let ty = edit_rect.y0 + (edit_rect.height() - self.metrics.cell_line_h) / 2.0;
        let tx = edit_rect.x0 + CELL_PAD_X;
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            transform,
            &edit_rect,
        );
        // Selection highlight behind the text.
        if let Some((sel_s, sel_e)) = e.selection() {
            let chars: Vec<char> = text.chars().collect();
            let sel_s = sel_s.min(chars.len());
            let sel_e = sel_e.min(chars.len());
            let pre: String = chars.get(..sel_s).unwrap_or_default().iter().collect();
            let mid: String = chars.get(sel_s..sel_e).unwrap_or_default().iter().collect();
            let (x0w, _) = tr.measure(&pre, size, None);
            let (midw, _) = tr.measure(&mid, size, None);
            let sel_rect = Rect::new(
                tx + f64::from(x0w),
                edit_rect.y0 + 3.0,
                tx + f64::from(x0w) + f64::from(midw),
                edit_rect.y1 - 3.0,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                colors.accent[5].with_alpha_factor(0.30),
                None,
                &sel_rect,
            );
        }
        let caret_x = draw_formula_line(
            scene,
            transform,
            tr,
            &text,
            size,
            theme,
            Point::new(tx, ty),
            Some(e.caret()),
        );
        scene.stroke(
            &Stroke::new(1.5),
            transform,
            colors.accent[5],
            None,
            &Line::new((caret_x, edit_rect.y0 + 4.0), (caret_x, edit_rect.y1 - 4.0)),
        );
        scene.pop_layer();
        scene.pop_layer();
    }
}

/// The colour of the `slot`-th reference in a formula, so each referenced
/// cell/range reads in its own colour both in the formula text and as an
/// outline on the grid — the coloured-reference cue every desktop
/// spreadsheet shows while editing a formula. The theme's chart series
/// (accent plus semantic hues) is made legible as text on the field and
/// cell surface both are drawn on.
pub(crate) fn reference_color(theme: &Theme, slot: usize) -> Color {
    let colors = &theme.tokens.colors;
    colors.ink_on(
        colors.surfaces.base,
        ChartPalette::from_theme(theme).categorical(slot),
        ON_FILL_MIN_CONTRAST,
    )
}

/// Draw a formula string with each cell/range reference tinted its assigned
/// colour (the rest in `text_primary`), starting at `origin` (top-left of the
/// text). When `caret` is `Some`, the returned value is the caret's x in
/// scene space so the caller can draw a matching caret; otherwise the return
/// is the x just past the text. Only strings beginning with `=` are scanned
/// for references — a plain value is drawn entirely in `text_primary`.
pub(crate) fn draw_formula_line(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    text: &str,
    size: f32,
    theme: &Theme,
    origin: Point,
    caret: Option<usize>,
) -> f64 {
    let base_color = theme.tokens.colors.text_primary;
    let chars: Vec<char> = text.chars().collect();
    // (start_char, end_char, colour) segments in full-text char space.
    let mut segs: Vec<(usize, usize, Color)> = Vec::new();
    if chars.first() == Some(&'=') {
        let body: String = chars.get(1..).unwrap_or_default().iter().collect();
        let spans = crate::formula::references(&body);
        let slots = crate::formula::ref_color_slots(&spans);
        let mut cursor = 0usize;
        for (span, slot) in spans.iter().zip(&slots) {
            let s = span.start + 1;
            let e = span.end + 1;
            if s > cursor {
                segs.push((cursor, s, base_color));
            }
            segs.push((s, e, reference_color(theme, *slot)));
            cursor = e;
        }
        if cursor < chars.len() {
            segs.push((cursor, chars.len(), base_color));
        }
    } else if !chars.is_empty() {
        segs.push((0, chars.len(), base_color));
    }

    let mut x = origin.x;
    let mut caret_x = origin.x;
    for (s, e, color) in segs {
        if let Some(c) = caret
            && c >= s
            && c <= e
        {
            let prefix: String = chars.get(s..c).unwrap_or_default().iter().collect();
            let (pw, _) = tr.measure(&prefix, size, None);
            caret_x = x + f64::from(pw);
        }
        let seg: String = chars.get(s..e).unwrap_or_default().iter().collect();
        tr.draw_text(
            scene,
            &seg,
            size,
            color,
            transform * Affine::translate((x, origin.y)),
            None,
        );
        let (sw, _) = tr.measure(&seg, size, None);
        x += f64::from(sw);
    }
    caret_x
}

/// Intersection of two rects, or `None` if they don't overlap.
fn intersect(a: Rect, b: Rect) -> Option<Rect> {
    let x0 = a.x0.max(b.x0);
    let y0 = a.y0.max(b.y0);
    let x1 = a.x1.min(b.x1);
    let y1 = a.y1.min(b.y1);
    if x1 > x0 && y1 > y0 {
        Some(Rect::new(x0, y0, x1, y1))
    } else {
        None
    }
}

/// A small, active-cell-centred interval used before the grid has received a
/// layout. Saturating arithmetic keeps an outlying active cell inside the
/// published slice without making a startup accessibility request expensive.
fn bounded_window(active: u32, total: u32, limit: u32) -> Vec<u32> {
    if total == 0 || limit == 0 {
        return Vec::new();
    }
    let take = total.min(limit);
    let half = take / 2;
    let start = active
        .min(total.saturating_sub(1))
        .saturating_sub(half)
        .min(total.saturating_sub(take));
    (start..start.saturating_add(take)).collect()
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::*;
    use crate::model::CellValue;

    #[test]
    fn formula_reference_colours_follow_the_theme() {
        let dark = Theme::dark();
        let other = Theme::named("Gruvbox").expect("Gruvbox is a shipped preset");
        let first = reference_color(&dark, 0);
        let second = reference_color(&dark, 1);
        assert_ne!(first, second, "consecutive references stay distinct");
        let wrap = ChartPalette::from_theme(&dark).len();
        assert_eq!(
            reference_color(&dark, wrap),
            first,
            "the series wraps with the theme palette"
        );
        assert_ne!(
            first,
            reference_color(&other, 0),
            "the first slot is grown from the theme, not a fixed hue"
        );
    }

    /// Measured metrics and draggable scrollbar thumbs.
    #[test]
    fn grid_metrics_and_scrollbars_follow_the_renderer() {
        // Case `measured_metrics_drive_automatic_geometry_but_never_manual_sizes`.
        // Metrics resolved against a real renderer at 1× reproduce the classic
        // density; at 2× the automatic row, header height and row-header width
        // follow the measured text, the prefix sums are rebuilt from them, and a
        // user's manual row height survives untouched.
        {
            let mut tr = TextRenderer::new();
            let one_x = GridMetrics::resolve(&mut tr, &Theme::dark());
            assert!(
                (one_x.default_row_h() - DEFAULT_ROW_H).abs() < 0.01,
                "1x row"
            );
            assert!(
                (one_x.col_header_h() - COL_HEADER_H).abs() < 0.01,
                "1x header"
            );
            assert!(
                (one_x.row_header_w() - ROW_HEADER_W).abs() < 0.01,
                "1x row header"
            );
            assert!(
                (one_x.cell_size - 14.0).abs() < 0.01,
                "cell text is the base token"
            );

            let mut g = Grid::new();
            g.row_heights.insert(3, 40.0);
            g.rebuild_geom();
            let one_x_top_of_row_2 = g.row_top(2);

            let theme = Theme::dark().with_text_scale(2.0);
            tr.set_text_scale(2.0);
            let two_x = GridMetrics::resolve(&mut tr, &theme);
            // The renderer scales its input once, so the input size is unchanged
            // while the measured line box doubled.
            assert!((two_x.cell_size - 14.0).abs() < 0.01);
            assert!(two_x.cell_line_h > one_x.cell_line_h * 1.9);
            assert!(two_x.default_row_h() >= two_x.cell_line_h + two_x.pad - 0.01);
            assert!(two_x.col_header_h() >= two_x.header_line_h + two_x.header_pad - 0.01);
            assert!(two_x.row_header_w() >= two_x.row_label_w + 2.0 * two_x.pad - 0.01);

            g.set_metrics(two_x);
            assert!(
                (g.row_h(3) - 40.0).abs() < 0.01,
                "manual row height is byte-for-byte"
            );
            assert!(
                (g.row_h(4) - two_x.default_row_h()).abs() < 0.01,
                "automatic row follows text"
            );
            assert!(
                g.row_top(2) > one_x_top_of_row_2,
                "prefix sums were rebuilt"
            );
            assert!((g.row_top(4) - (3.0 * two_x.default_row_h() + 40.0)).abs() < 0.01);
            assert!((g.grid_y0() - g.rect.y0 - two_x.col_header_h()).abs() < 0.01);
            assert!((g.grid_x0() - g.rect.x0 - two_x.row_header_w()).abs() < 0.01);
            // Unchanged metrics are a no-op: no needless geometry rebuild flag.
            g.needs_autofit = false;
            g.set_metrics(two_x);
            assert!(!g.needs_autofit);
        }

        // Case `visible_scrollbar_thumb_is_pointer_draggable_on_both_axes`.
        {
            let mut grid = grid_with(&[]);
            // The normal logical grid is larger than this deliberately narrow
            // viewport, so both rails expose a thumb and have scroll range.
            grid.rect = Rect::new(0.0, 0.0, 260.0, 180.0);
            let mut out = Vec::new();
            let horizontal = grid.scrollbar_thumb(ScrollAxis::Horizontal);
            assert!(grid.pointer_down(
                Point::new(
                    horizontal.x0.midpoint(horizontal.x1),
                    horizontal.y0.midpoint(horizontal.y1)
                ),
                false,
                false,
                &mut out,
            ));
            assert!(grid.pointer_move(Point::new(
                horizontal.x1 + 40.0,
                horizontal.y0.midpoint(horizontal.y1)
            )));
            grid.pointer_up();
            assert!(grid.scroll_x > 0.0);

            let vertical = grid.scrollbar_thumb(ScrollAxis::Vertical);
            assert!(grid.pointer_down(
                Point::new(
                    vertical.x0.midpoint(vertical.x1),
                    vertical.y0.midpoint(vertical.y1)
                ),
                false,
                false,
                &mut out,
            ));
            assert!(grid.pointer_move(Point::new(
                vertical.x0.midpoint(vertical.x1),
                vertical.y1 + 40.0
            )));
            grid.pointer_up();
            assert!(grid.scroll_y > 0.0);
        }
    }

    /// A grid with a known geometry: each column is the default width, the
    /// body starts at `ROW_HEADER_W` / `COL_HEADER_H` and is wide enough for
    /// many columns. No scroll.
    fn grid_with(values: &[(Addr, CellValue)]) -> Grid {
        let mut g = Grid::new();
        // Body roughly 12 columns wide so overflow has room.
        let width = ROW_HEADER_W + DEFAULT_COL_W * 12.0;
        let height = COL_HEADER_H + DEFAULT_ROW_H * 12.0;
        g.rect = Rect::new(0.0, 0.0, width, height);
        let mut map = HashMap::new();
        for (addr, v) in values {
            map.insert(*addr, v.clone());
        }
        g.values = map;
        g
    }

    /// Occupancy detection and autofit.
    #[test]
    fn autofit_respects_occupancy_and_manual_sizes() {
        // Case `occupied_detects_value_and_raw`.
        {
            let mut g = grid_with(&[((1, 0), CellValue::Number(5.0))]);
            assert!(g.cell_is_occupied((1, 0)));
            assert!(!g.cell_is_occupied((2, 0)));
            // Empty CellValue is not occupied.
            g.values.insert((3, 0), CellValue::Empty);
            assert!(!g.cell_is_occupied((3, 0)));
            // A raw entry (mid-edit, pre-eval) counts as occupied too.
            g.raw.insert((4, 0), "x".into());
            assert!(g.cell_is_occupied((4, 0)));
        }

        // Case `autofit_grows_column_to_fit_label_past_occupied_neighbour`.
        {
            // A0 holds a label wider than a default column; B0 is occupied so the
            // label can't overflow into it. Auto-fit must widen column A to fit the
            // whole label, while the short occupied neighbour keeps the default.
            let label = "Monthly budget total";
            let mut g = grid_with(&[
                ((0, 0), CellValue::Text(label.into())),
                ((1, 0), CellValue::Number(2600.0)),
            ]);
            let mut tr = TextRenderer::new();
            let size = 15.0;
            let label_w = f64::from(tr.measure(label, size, None).0);
            let want = label_w + CELL_PAD_X * 2.0;
            // Precondition: this label really does overflow a default-width column.
            assert!(
                want > DEFAULT_COL_W,
                "test label must exceed the default width"
            );

            g.autofit_columns(&mut tr, size);

            assert!(
                g.col_w(0) + 0.5 >= want,
                "col A width {} must fit the label (needs {want})",
                g.col_w(0),
            );
            assert!(g.col_w(0) <= MAX_AUTOFIT_W + 0.5, "auto-fit stays clamped");
            assert_eq!(
                g.col_w(1),
                DEFAULT_COL_W,
                "short occupied neighbour is left alone"
            );
        }

        // Case `autofit_leaves_manually_resized_columns_untouched`.
        {
            // A manual resize must win over auto-fit.
            let mut g = grid_with(&[((0, 0), CellValue::Text("Monthly budget total".into()))]);
            g.col_widths.insert(0, 200.0);
            let mut tr = TextRenderer::new();
            g.autofit_columns(&mut tr, 15.0);
            assert_eq!(g.col_w(0), 200.0, "manual width overrides auto-fit");
        }
    }

    /// In-cell editor overflow and clipping.
    #[test]
    fn cell_editor_overflows_only_into_empty_neighbours() {
        // Case `editor_overflows_into_empty_neighbours`.
        {
            // A0 is being edited; B0.. are all empty. A wide desired width should
            // grow across several empty columns.
            let g = grid_with(&[]);
            let cell = g.cell_screen_rect(0, 0);
            let body_right = g.body_rect().x1;
            // Desired width spanning ~3 columns.
            let desired = DEFAULT_COL_W * 3.0;
            let right = g.editor_right_extent((0, 0), desired, body_right);
            // It should reach at least 3 columns wide (within the body).
            assert!(
                right >= cell.x0 + DEFAULT_COL_W * 3.0 - 0.5,
                "right={right} expected >= {}",
                cell.x0 + DEFAULT_COL_W * 3.0
            );
            assert!(right <= body_right + 0.5);
        }

        // Case `editor_clips_at_first_occupied_neighbour`.
        {
            // A0 edited; C0 (two cells over) is occupied. The editor may use B0
            // but must STOP before C0 even though it wants more room.
            let g = grid_with(&[((2, 0), CellValue::Text("blocker".into()))]);
            let _cell = g.cell_screen_rect(0, 0);
            let body_right = g.body_rect().x1;
            let desired = DEFAULT_COL_W * 6.0; // wants a lot
            let right = g.editor_right_extent((0, 0), desired, body_right);
            // Reaches the end of B0 (= start of C0) but not into C0.
            let c0_left = g.cell_screen_rect(2, 0).x0;
            assert!(
                (right - c0_left).abs() < 0.5,
                "right={right} c0_left={c0_left}"
            );
        }

        // Case `editor_at_least_its_own_cell_when_neighbour_adjacent`.
        {
            // B0 occupied directly to the right of A0 → editor stays cell-wide.
            let g = grid_with(&[((1, 0), CellValue::Number(1.0))]);
            let cell = g.cell_screen_rect(0, 0);
            let body_right = g.body_rect().x1;
            let right = g.editor_right_extent((0, 0), DEFAULT_COL_W * 4.0, body_right);
            assert!(
                (right - cell.x1).abs() < 0.5,
                "right={right} cell.x1={}",
                cell.x1
            );
        }

        // Case `editor_never_exceeds_body`.
        {
            // Edit the last visible column with a huge desired width.
            let g = grid_with(&[]);
            let body_right = g.body_rect().x1;
            let right = g.editor_right_extent((10, 0), DEFAULT_COL_W * 50.0, body_right);
            assert!(
                right <= body_right + 0.5,
                "right={right} body_right={body_right}"
            );
        }
    }

    /// The screen point at the centre of a cell — where a click would land.
    fn cell_point(g: &Grid, c: u32, r: u32) -> Point {
        g.cell_screen_rect(c, r).center()
    }

    /// Drag, shift/ctrl click/drag multi-ranges, aggregation and arrow collapse.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn pointer_selection_builds_and_collapses_ranges() {
        // Case `drag_selects_a_rectangular_range`.
        {
            // Press on B2, move to D5 with the button held, release. This is the
            // exact `pointer_down`/`pointer_move`/`pointer_up` path `handle_event`
            // drives on a live left-drag.
            let mut g = grid_with(&[]);
            let mut out = Vec::new();
            assert!(g.pointer_down(cell_point(&g, 1, 1), false, false, &mut out));
            // Live update on each move while the button is held.
            g.pointer_move(cell_point(&g, 2, 3));
            assert_eq!(g.selection_bounds(), ((1, 1), (2, 3)), "mid-drag extent");
            g.pointer_move(cell_point(&g, 3, 4));
            g.pointer_up();
            assert_eq!(g.selection_bounds(), ((1, 1), (3, 4)));
            assert_eq!(g.active(), (3, 4));
            assert!(!g.is_multi_range());
        }

        // Case `shift_click_extends_from_anchor`.
        {
            let mut g = grid_with(&[]);
            let mut out = Vec::new();
            g.pointer_down(cell_point(&g, 1, 1), false, false, &mut out);
            g.pointer_up();
            // Shift+click D4: anchor stays at B2, range grows to B2:D4.
            g.pointer_down(cell_point(&g, 3, 3), true, false, &mut out);
            g.pointer_up();
            assert_eq!(g.selection_bounds(), ((1, 1), (3, 3)));
            assert!(!g.is_multi_range());
        }

        // Case `ctrl_click_adds_a_discontiguous_range`.
        {
            let mut g = grid_with(&[]);
            let mut out = Vec::new();
            // First range: drag B2:C3.
            g.pointer_down(cell_point(&g, 1, 1), false, false, &mut out);
            g.pointer_move(cell_point(&g, 2, 2));
            g.pointer_up();
            // Ctrl+click F6 → adds a second, single-cell range; F6 becomes active.
            g.pointer_down(cell_point(&g, 5, 5), false, true, &mut out);
            g.pointer_up();
            assert!(g.is_multi_range());
            assert_eq!(g.active(), (5, 5));
            let cells = g.selected_cells();
            assert!(
                cells.contains(&(1, 1)) && cells.contains(&(2, 2)),
                "first range kept"
            );
            assert!(cells.contains(&(5, 5)), "second range added");
            assert!(!cells.contains(&(3, 3)), "gap between ranges not selected");
        }

        // Case `ctrl_drag_adds_a_second_rectangle`.
        {
            let mut g = grid_with(&[]);
            let mut out = Vec::new();
            // First range: a single cell B2.
            g.pointer_down(cell_point(&g, 1, 1), false, false, &mut out);
            g.pointer_up();
            // Ctrl+drag F6→G8 adds a new rectangle while keeping B2.
            g.pointer_down(cell_point(&g, 5, 5), false, true, &mut out);
            g.pointer_move(cell_point(&g, 6, 7));
            g.pointer_up();
            assert!(g.is_multi_range());
            let cells = g.selected_cells();
            assert!(cells.contains(&(1, 1)), "original cell kept");
            for c in 5..=6 {
                for r in 5..=7 {
                    assert!(cells.contains(&(c, r)), "added rectangle covers ({c},{r})");
                }
            }
        }

        // Case `multi_range_selection_aggregates_numeric_cells_over_the_real_pointer_path`.
        {
            // Two numeric columns with a non-numeric gap between them. Column B
            // (index 1) holds 100/200/300; column D (index 3) holds 10/20/30; the
            // gap column C (index 2) holds a label so it must never contribute.
            let mut g = grid_with(&[
                ((1, 1), CellValue::Number(100.0)),
                ((1, 2), CellValue::Number(200.0)),
                ((1, 3), CellValue::Number(300.0)),
                ((2, 1), CellValue::Text("label".into())),
                ((3, 1), CellValue::Number(10.0)),
                ((3, 2), CellValue::Number(20.0)),
                ((3, 3), CellValue::Number(30.0)),
            ]);
            let mut out = Vec::new();
            // Drag-select B2:B4 (the first rectangle), then Ctrl+drag D2:D4 to add a
            // discontiguous second rectangle — the exact production pointer path.
            g.pointer_down(cell_point(&g, 1, 1), false, false, &mut out);
            g.pointer_move(cell_point(&g, 1, 3));
            g.pointer_up();
            g.pointer_down(cell_point(&g, 3, 1), false, true, &mut out);
            g.pointer_move(cell_point(&g, 3, 3));
            g.pointer_up();
            assert!(g.is_multi_range(), "two discontiguous ranges are selected");

            // Aggregate over the exact methods the status bar's summary consumes:
            // `selected_cells()` (dedup across ranges) and `cell_value_at()`.
            let mut sum = 0.0;
            let mut count = 0usize;
            for addr in g.selected_cells() {
                if let Some(CellValue::Number(n)) = g.cell_value_at(addr) {
                    sum += n;
                    count += 1;
                }
            }
            assert_eq!(count, 6, "six numeric cells across both rectangles");
            assert!(
                (sum - 660.0).abs() < 1e-9,
                "sum over both ranges = 660, got {sum}"
            );
            // The gap column's label is inside neither rectangle.
            assert!(!g.selected_cells().contains(&(2, 1)), "gap column excluded");
        }

        // Case `arrow_key_collapses_multi_selection`.
        {
            let mut g = grid_with(&[]);
            let mut out = Vec::new();
            g.pointer_down(cell_point(&g, 1, 1), false, false, &mut out);
            g.pointer_up();
            g.pointer_down(cell_point(&g, 5, 5), false, true, &mut out);
            g.pointer_up();
            assert!(g.is_multi_range());
            // A bare arrow key drops the extra ranges back to a single active cell.
            g.move_active(1, 0, false);
            assert!(!g.is_multi_range());
            assert_eq!(g.selected_cells(), vec![(6, 5)]);
        }
    }

    // --- formula point mode (build formulas by clicking) --------------------

    /// Point-mode click/drag insertion, operator append, replacement and off-state.
    #[test]
    fn formula_point_mode_inserts_references() {
        // Case `formula_point_click_inserts_reference_without_moving_selection`.
        {
            // Select A5, start a formula (as typing '=' would), then click B1.
            // This drives the exact grid path `handle_event` runs on a click.
            let mut g = grid_with(&[]);
            let mut out = Vec::new();
            g.select_only((0, 4));
            g.begin_edit(Some("=".into()));
            assert!(g.pointer_down(cell_point(&g, 1, 0), false, false, &mut out));
            g.pointer_up();
            assert!(
                out.is_empty(),
                "a point-mode click must not commit the edit"
            );
            assert_eq!(g.active_display_raw(), "=B1", "clicked cell's ref inserted");
            assert!(g.is_editing(), "still editing the original cell");
            assert_eq!(g.active(), (0, 4), "selection stayed on the edited cell");
        }

        // Case `formula_point_drag_inserts_range`.
        {
            // Press A1, drag to B4 while editing a formula → inserts A1:B4.
            let mut g = grid_with(&[]);
            let mut out = Vec::new();
            g.select_only((0, 6));
            g.begin_edit(Some("=".into()));
            assert!(g.pointer_down(cell_point(&g, 0, 0), false, false, &mut out));
            g.pointer_move(cell_point(&g, 1, 3));
            g.pointer_up();
            assert_eq!(g.active_display_raw(), "=A1:B4");
            assert!(out.is_empty(), "the drag never committed the edit");
        }

        // Case `formula_point_appends_after_operator`.
        {
            // =A1  then type '+'  then click B2  →  =A1+B2.
            let mut g = grid_with(&[]);
            let mut out = Vec::new();
            g.select_only((4, 4));
            g.begin_edit(Some("=".into()));
            g.pointer_down(cell_point(&g, 0, 0), false, false, &mut out);
            g.pointer_up();
            assert_eq!(g.active_display_raw(), "=A1");
            g.insert_text_public("+");
            g.pointer_down(cell_point(&g, 1, 1), false, false, &mut out);
            g.pointer_up();
            assert_eq!(g.active_display_raw(), "=A1+B2");
        }

        // Case `formula_second_click_replaces_unoperated_reference`.
        {
            // Click A1, then C3 without typing an operator → the ref is replaced
            // (Excel point-mode behaviour), not appended.
            let mut g = grid_with(&[]);
            let mut out = Vec::new();
            g.select_only((4, 4));
            g.begin_edit(Some("=".into()));
            g.pointer_down(cell_point(&g, 0, 0), false, false, &mut out);
            g.pointer_up();
            g.pointer_down(cell_point(&g, 2, 2), false, false, &mut out);
            g.pointer_up();
            assert_eq!(g.active_display_raw(), "=C3");
        }

        // Case `point_mode_off_when_caret_after_a_value`.
        {
            // A non-pointable click (nothing typed makes the caret ready) commits
            // the edit and moves the selection instead of inserting a reference.
            let mut g = grid_with(&[]);
            let mut out = Vec::new();
            g.select_only((0, 0));
            g.begin_edit(Some("=A1".into())); // caret after '1', no operator
            g.point_anchor = None; // as if the ref were typed, not clicked
            assert!(g.pointer_down(cell_point(&g, 3, 3), false, false, &mut out));
            g.pointer_up();
            // The edit committed to A1 and the selection moved to the clicked cell.
            assert!(
                out.iter().any(|c| matches!(c, GridCmd::SetCell { .. })),
                "a non-pointable click commits the edit"
            );
            assert!(!g.is_editing());
            assert_eq!(g.active(), (3, 3));
        }
    }

    // --- formula-bar text editing (caret + selection) ----------------------

    /// Edit selection replace/paste, grapheme boundaries, cut and shift-arrow delete.
    #[test]
    fn cell_edit_selection_replaces_cuts_and_deletes() {
        // Case `edit_selection_replace_and_paste`.
        {
            // The exact methods the formula bar drives: set_edit_caret (click /
            // shift-drag), typing over a selection, select-all, and paste.
            let mut g = grid_with(&[]);
            g.select_only((0, 0));
            g.begin_edit(Some("hello".into()));
            g.set_edit_caret(1, false); // click after 'h'
            g.set_edit_caret(4, true); // shift-drag to select "ell"
            assert_eq!(g.edit_selection(), Some((1, 4)));
            assert_eq!(g.edit_selected_text().as_deref(), Some("ell"));
            // Typing replaces the selection.
            g.insert_text_public("ELL");
            assert_eq!(g.active_display_raw(), "hELLo");
            // Ctrl+A then paste replaces the whole field.
            g.select_all_edit();
            g.edit_paste_text("=A1+B2");
            assert_eq!(g.active_display_raw(), "=A1+B2");
        }

        // Case `shared_edit_core_keeps_unicode_selection_on_grapheme_boundaries`.
        {
            let mut g = Grid::new();
            g.begin_edit(Some("a🙂b".into()));
            g.set_edit_caret(1, false);
            g.set_edit_caret(2, true);
            assert_eq!(g.edit_selected_text().as_deref(), Some("🙂"));
            g.insert_text_public("X");
            assert_eq!(g.edit_selected_text(), None);
            assert_eq!(g.active_display_raw(), "aXb");
        }

        // Case `edit_cut_removes_selection`.
        {
            let mut g = grid_with(&[]);
            g.select_only((0, 0));
            g.begin_edit(Some("abcdef".into()));
            g.set_edit_caret(2, false);
            g.set_edit_caret(5, true); // select "cde"
            assert_eq!(g.edit_cut_text().as_deref(), Some("cde"));
            assert_eq!(g.active_display_raw(), "abf");
            assert_eq!(g.edit_caret(), Some(2));
        }

        // Case `shift_arrow_extends_then_backspace_deletes_selection`.
        {
            let mut g = grid_with(&[]);
            g.select_only((0, 0));
            g.begin_edit(Some("abcd".into())); // caret at end (4)
            g.edit_move_caret(CaretMove::Left, true); // select "d"
            g.edit_move_caret(CaretMove::Left, true); // select "cd"
            assert_eq!(g.edit_selection(), Some((2, 4)));
            // Backspace removes the whole selection, not one char.
            assert!(g.delete_selection());
            assert_eq!(g.active_display_raw(), "ab");
        }
    }

    /// Formula/header highlights and bounded accessibility windows.
    #[test]
    fn highlights_and_accessibility_windows_track_the_selection() {
        // Case `selected_formula_cell_highlights_without_editing`.
        {
            let mut g = grid_with(&[]);
            g.raw.insert((0, 0), "=A2+A3".into());
            g.select_only((0, 0));
            assert!(!g.is_editing());
            // Refs are available for highlighting even when only selecting.
            assert_eq!(g.highlight_formula_body().as_deref(), Some("A2+A3"));
            // A plain value cell has nothing to highlight.
            g.raw.insert((1, 0), "42".into());
            g.select_only((1, 0));
            assert_eq!(g.highlight_formula_body(), None);
        }

        // Case `header_highlight_tracks_selected_columns_and_rows`.
        {
            let mut g = grid_with(&[]);
            g.select_range_public((2, 3), (4, 5)); // C4:E6
            assert!(g.col_in_selection(2) && g.col_in_selection(4));
            assert!(!g.col_in_selection(1) && !g.col_in_selection(5));
            assert!(g.row_in_selection(3) && g.row_in_selection(5));
            assert!(!g.row_in_selection(2) && !g.row_in_selection(6));
            // A discontiguous range lights up its columns/rows too.
            g.add_range_public((7, 8), (7, 8)); // H9
            assert!(g.col_in_selection(7) && g.row_in_selection(8));
            assert!(g.col_in_selection(2), "original range headers stay lit");
        }

        // Case `accessibility_window_is_viewport_bounded_and_marks_selection`.
        {
            let mut g = grid_with(&[((1, 1), CellValue::Number(42.0))]);
            g.raw.insert((1, 1), "42".into());
            g.select_only((1, 1));
            let window = g.accessibility_window();
            assert!(window.columns.len() < g.n_cols as usize);
            assert!(window.rows.len() < g.n_rows as usize);
            let active = window
                .rows
                .iter()
                .flat_map(|row| &row.cells)
                .find(|cell| cell.addr == (1, 1))
                .expect("visible active cell");
            assert_eq!(active.display, "42");
            assert!(active.selected);
        }

        // Case `accessibility_window_before_layout_stays_small_near_active_cell`.
        {
            let mut g = Grid::new();
            g.select_only((40, 180));
            let window = g.accessibility_window();
            assert!(window.columns.len() <= A11Y_FALLBACK_COLS as usize);
            assert!(window.rows.len() <= A11Y_FALLBACK_ROWS as usize);
            assert!(
                window
                    .rows
                    .iter()
                    .flat_map(|row| &row.cells)
                    .any(|cell| cell.addr == (40, 180))
            );
        }

        // Case `visible_window_at_workbook_limits_is_small_and_reaches_the_end`.
        {
            let mut g = Grid::new();
            g.n_cols = 16_384;
            g.n_rows = 1_048_576;
            g.rebuild_geom();
            g.rect = Rect::new(0.0, 0.0, 280.0, 180.0);
            g.scroll_x = g.max_scroll_x();
            g.scroll_y = g.max_scroll_y();

            let columns = g.visible_cols();
            let rows = g.visible_rows();
            assert!(columns.len() < 8 && rows.len() < 12);
            assert_eq!(columns.last().copied(), Some(g.n_cols - 1));
            assert_eq!(rows.last().copied(), Some(g.n_rows - 1));
        }
    }
}
