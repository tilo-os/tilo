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
use winit::event::{Ime, Modifiers, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::chart_palette::ChartPalette;
use ui::paint::PaintCtx;
use ui::surface::Region;
use ui::text_edit::{EditCore, Motion};
use ui::text_render::{TextRenderer, TextStyle};
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::tokens::colors::ON_FILL_MIN_CONTRAST;
use ui::widget::ControlSize;
use ui::widget::scrollbar::{ScrollMetrics, Scrollbar};

use crate::model::{Addr, CellFormat, CellValue, addr_to_a1, format_value};

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

/// The formula bar's text and, while editing, the caret, selection and
/// input-method composition as char indices into it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FormulaBarText {
    pub text: String,
    pub caret: Option<usize>,
    pub selection: Option<(usize, usize)>,
    pub preedit: Option<(usize, usize)>,
}

#[derive(Clone)]
struct EditState {
    addr: Addr,
    text: String,
    /// Shared caret/selection core. The grid keeps its custom cell painting
    /// and point-mode formula insertion, but all text editing semantics come
    /// from the same byte-safe core as the other editable surfaces.
    core: EditCore,
    /// Started by typing over the cell rather than by F2, double-click or the
    /// formula bar. As in other spreadsheets, arrow keys then finish the entry
    /// and move to the neighbouring cell instead of moving the caret; placing
    /// the caret explicitly switches to caret editing.
    entering: bool,
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
        if !edit.is_noop() && edit.apply(&mut self.text) {
            self.core.clamp_to(&self.text);
        }
    }

    /// The text as drawn: the committed text with any input-method
    /// composition shown at the caret.
    fn shown(&self) -> std::borrow::Cow<'_, str> {
        self.core.display_text(&self.text)
    }

    /// Char index into [`Self::shown`] of display byte `byte`.
    fn shown_chars(&self, byte: usize) -> usize {
        self.shown()
            .get(..byte)
            .map_or(0, |head| head.chars().count())
    }

    /// The caret as a char index into [`Self::shown`] — inside the
    /// composition at the input method's cursor while composing.
    fn shown_caret(&self) -> usize {
        self.shown_chars(self.core.display_caret(&self.text))
    }

    /// The selection as char indices into [`Self::shown`].
    fn shown_selection(&self) -> Option<(usize, usize)> {
        let range = self.core.selected_range()?;
        Some((
            self.shown_chars(self.core.to_display(&self.text, range.start)),
            self.shown_chars(self.core.to_display(&self.text, range.end)),
        ))
    }

    /// The composition as char indices into [`Self::shown`].
    fn shown_preedit(&self) -> Option<(usize, usize)> {
        let range = self.core.preedit_display_range(&self.text)?;
        Some((self.shown_chars(range.start), self.shown_chars(range.end)))
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

/// Text and band metrics for the grid, resolved from the theme and the
/// renderer that paints it.
///
/// Cells are `Body`, the row and column headers `Caption` (tabular figures),
/// so every band, row and header derives from a measured role line box plus
/// a spacing token or the small control height, and layout, paint and
/// hit-testing share one metric. Manual row heights and column widths are
/// never touched.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridMetrics {
    /// Accessibility text scale used to grow authored interaction floors.
    pub scale: f64,
    /// Measured line box of the cell text (`Body`), in final pixels.
    pub cell_line_h: f64,
    /// Measured line box of the header labels (`Caption`), in final pixels.
    pub header_line_h: f64,
    /// Measured width of a four-digit row label in the header role.
    pub row_label_w: f64,
    /// Theme spacing the bands pad with (`s2`, `s3`).
    pub pad: f64,
    pub header_pad: f64,
    /// Height of the smallest control: the floor of a row and of the header.
    pub control_h: f64,
    /// Spacing steps the column geometry is built from.
    pub s1: f64,
    pub s2: f64,
    pub s8: f64,
    pub s12: f64,
    pub s16: f64,
    /// Room a scroll view keeps on its trailing edge for its bar.
    pub gutter: f64,
}

impl GridMetrics {
    /// Resolve the grid metrics for `theme` against `tr`, the renderer that
    /// will paint the grid.
    pub fn resolve(tr: &mut TextRenderer, theme: &Theme) -> Self {
        let (_, cell_line_h) = tr.measure_style("Ag", None, &theme.text(TextRole::Body));
        let (row_label_w, header_line_h) =
            tr.measure_style("9999", None, &theme.text(TextRole::Caption).numeric());
        let sp = &theme.tokens.spacing;
        Self {
            scale: f64::from(theme.text_scale),
            cell_line_h: f64::from(cell_line_h),
            header_line_h: f64::from(header_line_h),
            row_label_w: f64::from(row_label_w),
            pad: f64::from(sp.s2),
            header_pad: f64::from(sp.s3),
            control_h: ControlSize::Small.height(theme),
            s1: f64::from(sp.s1),
            s2: f64::from(sp.s2),
            s8: f64::from(sp.s8),
            s12: f64::from(sp.s12),
            s16: f64::from(sp.s16),
            gutter: f64::from(theme.tokens.metrics.scrollbar_gutter),
        }
    }

    /// The metrics used before the first draw has a renderer, from the
    /// stock theme's spacing; every band lands on its floor.
    #[must_use]
    pub fn fallback() -> Self {
        let theme = Theme::dark();
        let sp = &theme.tokens.spacing;
        Self {
            scale: 1.0,
            cell_line_h: 20.0,
            header_line_h: 16.0,
            row_label_w: 28.0,
            pad: f64::from(sp.s2),
            header_pad: f64::from(sp.s3),
            control_h: ControlSize::Small.height(&theme),
            s1: f64::from(sp.s1),
            s2: f64::from(sp.s2),
            s8: f64::from(sp.s8),
            s12: f64::from(sp.s12),
            s16: f64::from(sp.s16),
            gutter: f64::from(theme.tokens.metrics.scrollbar_gutter),
        }
    }

    /// Height of an automatically sized row: the cell line box plus `s2`,
    /// never below the small control height.
    #[must_use]
    pub fn default_row_h(&self) -> f64 {
        (self.cell_line_h + self.pad).max(self.control_h)
    }

    /// Height of the column-header strip: the header line box plus `s3`,
    /// never below the small control height.
    #[must_use]
    pub fn col_header_h(&self) -> f64 {
        (self.header_line_h + self.header_pad).max(self.control_h)
    }

    /// Width of the row-header strip: a four-digit label plus `s2` each side,
    /// never below `s12 + s2`.
    #[must_use]
    pub fn row_header_w(&self) -> f64 {
        (self.row_label_w + 2.0 * self.pad).max((self.s12 + self.s2) * self.scale)
    }

    /// Width of a column nobody sized: `s16 + s12`.
    #[must_use]
    pub fn default_col_w(&self) -> f64 {
        (self.s16 + self.s12) * self.scale
    }

    /// Widest an auto-fitted column grows, so one very long value cannot
    /// blow a column out to an unreadable width.
    #[must_use]
    pub fn max_autofit_w(&self) -> f64 {
        self.default_col_w() * 3.0
    }

    /// Narrowest a column can be dragged to.
    #[must_use]
    pub fn min_col_w(&self) -> f64 {
        self.s12 * self.scale
    }

    /// Shortest a row can be dragged to.
    #[must_use]
    pub fn min_row_h(&self) -> f64 {
        (self.s8 - self.s2) * self.scale
    }

    /// Horizontal inset of a cell's text.
    #[must_use]
    pub fn cell_pad(&self) -> f64 {
        self.pad * self.scale
    }

    /// How far a header edge's resize grab reaches either side.
    #[must_use]
    pub fn resize_grab(&self) -> f64 {
        self.s1 * self.scale
    }

    /// Side of the fill handle square at the selection's corner.
    #[must_use]
    pub fn fill_handle(&self) -> f64 {
        self.s2 * self.scale
    }

    /// Margin round a scrollbar's thumb inside its gutter.
    #[must_use]
    pub fn bar_margin(&self) -> f64 {
        // Keep a generous input gutter, but only 5–6 logical pixels of ink.
        (self.gutter - self.s1.clamp(5.0, 6.0)).max(0.0) / 2.0
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
    /// columns with neither fall back to the default column width.
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
    /// The library scroll bars' interaction state, one per axis.
    hbar: Scrollbar,
    vbar: Scrollbar,
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
            hbar: Scrollbar::horizontal(),
            vbar: Scrollbar::vertical(),
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

    /// Room the scroll bars keep along the body's right and bottom edges, so
    /// chrome floated over the grid can stay clear of them.
    #[must_use]
    pub fn scrollbar_thickness(&self) -> f64 {
        self.metrics.gutter
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
    /// selection. Used by paste (to select what landed) and screenshot staging.
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
    /// Stages a multi-range selection (Ctrl+drag equivalent).
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

    /// What the formula bar draws. While the active cell is being edited it
    /// is the cell editor's display text, so an input-method composition shows
    /// (and is underlined) in the bar exactly as in the cell.
    pub fn formula_bar(&self) -> FormulaBarText {
        let Some(e) = &self.edit else {
            return FormulaBarText {
                text: self.active_display_raw(),
                ..FormulaBarText::default()
            };
        };
        if e.addr != self.active {
            return FormulaBarText {
                text: self.active_display_raw(),
                caret: Some(e.caret()),
                selection: e.selection(),
                preedit: None,
            };
        }
        FormulaBarText {
            text: e.shown().into_owned(),
            caret: Some(e.shown_caret()),
            selection: e.shown_selection(),
            preedit: e.shown_preedit(),
        }
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

    /// Whether a cell's text is bold.
    pub fn cell_is_bold(&self, addr: Addr) -> bool {
        self.bold.contains(&addr)
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
            .unwrap_or_else(|| self.metrics.default_col_w())
    }
    fn row_h(&self, r: u32) -> f64 {
        self.row_heights
            .get(&r)
            .map_or_else(|| self.metrics.default_row_h(), |h| *h * self.text_scale)
    }

    /// Content-space x (before scroll) of column `c`'s left edge. O(1) via
    /// the cached prefix sums, with a defensive fallback past the last index.
    fn col_left(&self, c: u32) -> f64 {
        if let Some(&x) = self.col_x.get(c as usize) {
            x
        } else {
            let last = self.col_x.last().copied().unwrap_or(0.0);
            last + f64::from(c.saturating_sub(self.n_cols)) * self.metrics.default_col_w()
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

    /// The cell area: inside the headers and the scroll bar gutters, so the
    /// last row and the last column are never covered by a bar.
    pub(crate) fn body_rect(&self) -> Rect {
        let width = (self.rect.x1 - self.grid_x0()).max(0.0);
        let height = (self.rect.y1 - self.grid_y0()).max(0.0);
        let gutter = self.metrics.gutter;
        let mut horizontal = self.total_width() > width + ui::widget::scrollbar::FIT_EPS;
        let mut vertical = self.total_height() > height + ui::widget::scrollbar::FIT_EPS;
        // Either gutter can make the other axis overflow; settle both before
        // reserving space, without recursive body/scroll-metric queries.
        for _ in 0..2 {
            horizontal |= self.total_width()
                > width - if vertical { gutter } else { 0.0 } + ui::widget::scrollbar::FIT_EPS;
            vertical |= self.total_height()
                > height - if horizontal { gutter } else { 0.0 } + ui::widget::scrollbar::FIT_EPS;
        }
        Rect::new(
            self.grid_x0(),
            self.grid_y0(),
            self.rect.x1 - if vertical { gutter } else { 0.0 },
            self.rect.y1 - if horizontal { gutter } else { 0.0 },
        )
    }

    fn max_scroll_x(&self) -> f64 {
        (self.total_width() - self.body_rect().width()).max(0.0)
    }

    fn max_scroll_y(&self) -> f64 {
        (self.total_height() - self.body_rect().height()).max(0.0)
    }

    /// The gutter a bar lives in: where a press pages or grabs.
    fn bar_gutter(&self, axis: ScrollAxis) -> Rect {
        let body = self.body_rect();
        match axis {
            ScrollAxis::Horizontal => Rect::new(body.x0, body.y1, body.x1, self.rect.y1),
            ScrollAxis::Vertical => Rect::new(body.x1, body.y0, self.rect.x1, body.y1),
        }
    }

    /// The thumb's track: the gutter less a margin each side across the bar.
    fn bar_track(&self, axis: ScrollAxis) -> Rect {
        let margin = self.metrics.bar_margin();
        let gutter = self.bar_gutter(axis);
        match axis {
            ScrollAxis::Horizontal => gutter.inset((0.0, -margin)),
            ScrollAxis::Vertical => gutter.inset((-margin, 0.0)),
        }
    }

    fn bar_metrics(&self, axis: ScrollAxis) -> ScrollMetrics {
        match axis {
            ScrollAxis::Horizontal => {
                ScrollMetrics::new(self.total_width(), self.body_rect().width(), self.scroll_x)
            }
            ScrollAxis::Vertical => ScrollMetrics::new(
                self.total_height(),
                self.body_rect().height(),
                self.scroll_y,
            ),
        }
    }

    fn bar(&self, axis: ScrollAxis) -> &Scrollbar {
        match axis {
            ScrollAxis::Horizontal => &self.hbar,
            ScrollAxis::Vertical => &self.vbar,
        }
    }

    fn bar_mut(&mut self, axis: ScrollAxis) -> &mut Scrollbar {
        match axis {
            ScrollAxis::Horizontal => &mut self.hbar,
            ScrollAxis::Vertical => &mut self.vbar,
        }
    }

    fn set_scroll(&mut self, axis: ScrollAxis, offset: f64) {
        match axis {
            ScrollAxis::Horizontal => self.scroll_x = offset,
            ScrollAxis::Vertical => self.scroll_y = offset,
        }
        self.clamp_scroll();
        self.dirty = true;
    }

    /// A bar shows only while its content overflows the body.
    fn bar_shown(&self, axis: ScrollAxis) -> bool {
        self.bar_metrics(axis).overflows()
    }

    /// The thumb of an overflowing bar, for tests.
    #[cfg(test)]
    fn scrollbar_thumb(&self, axis: ScrollAxis) -> Option<Rect> {
        self.bar_shown(axis)
            .then(|| {
                self.bar(axis)
                    .thumb_rect(self.bar_track(axis), self.bar_metrics(axis))
            })
            .flatten()
    }

    fn grid_x0(&self) -> f64 {
        self.rect.x0 + self.metrics.row_header_w()
    }
    fn grid_y0(&self) -> f64 {
        self.rect.y0 + self.metrics.col_header_h()
    }

    /// Screen rect for a cell, in absolute coords (may lie outside the body).
    pub(crate) fn cell_screen_rect(&self, c: u32, r: u32) -> Rect {
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
        let m = self.metrics.fill_handle();
        if cx < body.x0 - m || cx > body.x1 + m || cy < body.y0 - m || cy > body.y1 + m {
            return None;
        }
        let h = m / 2.0;
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
    /// screenshot scenarios to exercise the real fill without a display.
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
                if (p.x - x).abs() <= self.metrics.resize_grab() {
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
                if (p.y - y).abs() <= self.metrics.resize_grab() {
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
        let entering = seed.is_some();
        let text = match seed {
            Some(s) => s,
            None => self.raw.get(&self.active).cloned().unwrap_or_default(),
        };
        let caret = text.len();
        self.edit = Some(EditState {
            addr: self.active,
            core: EditCore::at(caret),
            entering,
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

    /// Whether the edit was started by typing a value over the cell, so the
    /// arrow keys finish it and move on. Formulas keep the caret moving, since
    /// they are composed piecewise.
    fn entering_value(&self) -> bool {
        self.edit
            .as_ref()
            .is_some_and(|e| e.entering && !e.text.starts_with('='))
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
        use ui::widget::scrollbar::PressOutcome;
        for axis in [ScrollAxis::Horizontal, ScrollAxis::Vertical] {
            if !self.bar_shown(axis) || !self.bar_gutter(axis).contains(p) {
                continue;
            }
            let (gutter, metrics) = (self.bar_gutter(axis), self.bar_metrics(axis));
            match self.bar_mut(axis).press(gutter, metrics, p) {
                PressOutcome::Miss => continue,
                PressOutcome::Thumb => self.dirty = true,
                PressOutcome::Page(offset) => self.set_scroll(axis, offset),
            }
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
            let grab = hr.inflate(self.metrics.s1, self.metrics.s1);
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
            } else if !shift {
                self.extra_ranges.clear();
                self.anchor = addr;
            }
            // Shift extends the live range: it keeps its anchor and any other
            // ranges, and only the active cell moves.
            self.active = addr;
            self.selecting = true;
            self.dirty = true;
            return true;
        }
        false
    }

    pub fn pointer_move(&mut self, p: Point) -> bool {
        self.last_cursor = p;
        for axis in [ScrollAxis::Horizontal, ScrollAxis::Vertical] {
            if self.bar(axis).is_dragging() {
                let (gutter, metrics) = (self.bar_gutter(axis), self.bar_metrics(axis));
                if let Some(offset) = self.bar(axis).drag_to(gutter, metrics, p) {
                    self.set_scroll(axis, offset);
                }
                return true;
            }
        }
        for axis in [ScrollAxis::Horizontal, ScrollAxis::Vertical] {
            let (gutter, metrics) = (self.bar_gutter(axis), self.bar_metrics(axis));
            let over = self.bar_shown(axis)
                && self
                    .bar(axis)
                    .thumb_rect(gutter, metrics)
                    .is_some_and(|t| t.contains(p));
            if self.bar_mut(axis).set_hover(over) {
                self.dirty = true;
            }
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
                    let w =
                        (drag.origin_size + (p.x - drag.origin_px)).max(self.metrics.min_col_w());
                    self.col_widths.insert(c, w / self.text_scale);
                }
                ResizeTarget::Row(r) => {
                    let h =
                        (drag.origin_size + (p.y - drag.origin_px)).max(self.metrics.min_row_h());
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
        self.hbar.release();
        self.vbar.release();
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
                self.move_active(0, if shift { -1 } else { 1 }, false);
                true
            }
            Key::Named(NamedKey::Tab) => {
                self.commit_edit(out);
                self.move_active(if shift { -1 } else { 1 }, 0, false);
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
            Key::Named(
                key @ (NamedKey::ArrowLeft
                | NamedKey::ArrowRight
                | NamedKey::ArrowUp
                | NamedKey::ArrowDown),
            ) if !shift && self.entering_value() => {
                let (dc, dr) = match key {
                    NamedKey::ArrowLeft => (-1, 0),
                    NamedKey::ArrowRight => (1, 0),
                    NamedKey::ArrowUp => (0, -1),
                    _ => (0, 1),
                };
                self.commit_edit(out);
                self.move_active(dc, dr, false);
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

    /// An input-method event while a cell is being edited: the composition
    /// shows at the caret until it is committed into the cell like typing.
    /// Returns whether the edit took it.
    pub fn ime(&mut self, ime: &Ime) -> bool {
        // Text can arrive through the input method while only a cell is
        // selected (the page's text field may still hold focus from the last
        // edit, or a composition starts on a selected cell). It starts an
        // entry that replaces the cell, exactly like typing a key would.
        if self.edit.is_none() {
            match ime {
                Ime::Commit(text) if !text.is_empty() => self.begin_edit(Some(String::new())),
                Ime::Preedit(text, _) if !text.is_empty() => {
                    self.begin_edit(Some(String::new()));
                }
                _ => return false,
            }
        }
        let Some(e) = &mut self.edit else {
            return false;
        };
        match ime {
            Ime::Preedit(text, _) if text.is_empty() => e.core.clear_preedit(),
            Ime::Preedit(text, cursor) => e.core.set_preedit(text, *cursor),
            Ime::Commit(text) => {
                let edit = e.core.commit(text);
                e.apply_edit(&edit);
                self.point_anchor = None;
            }
            Ime::Disabled => e.core.clear_preedit(),
            Ime::Enabled => return false,
        }
        self.dirty = true;
        true
    }

    /// The edit as drawn — text with any composition, and the caret's char
    /// index into it.
    #[cfg(test)]
    pub(crate) fn edit_shown(&self) -> Option<(String, usize)> {
        let e = self.edit.as_ref()?;
        Some((e.shown().into_owned(), e.shown_caret()))
    }

    /// Where the input method's candidate window should sit: the cell being
    /// edited.
    pub fn ime_cell_rect(&self) -> Option<Rect> {
        let (col, row) = self.edit.as_ref()?.addr;
        Some(self.cell_screen_rect(col, row))
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
            e.entering = false;
            self.point_anchor = None;
            self.dirty = true;
        }
    }

    /// Move the edit caret to char index `idx` of the text the formula bar
    /// draws ([`Self::formula_bar`]). A click inside a composition lands at
    /// the caret it is composed at; past it, on the committed text it shows.
    pub fn set_edit_caret_shown(&mut self, idx: usize, extend: bool) {
        let Some(e) = &self.edit else {
            return;
        };
        if e.addr != self.active || !e.core.has_preedit() {
            self.set_edit_caret(idx, extend);
            return;
        }
        let shown = e.shown();
        let byte = shown
            .char_indices()
            .nth(idx)
            .map_or(shown.len(), |(b, _)| b);
        let committed = e.core.from_display(&e.text, byte);
        let idx = e
            .text
            .get(..committed)
            .map_or(0, |head| head.chars().count());
        self.set_edit_caret(idx, extend);
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

    /// The role a cell is set in: `Body`, or `Code` while formulas show as
    /// the source they were typed as. A number takes tabular figures and a
    /// bold cell the strong weight of its role.
    fn cell_style(&self, theme: &Theme, addr: Addr, numeric: bool) -> TextStyle {
        let mut style = if self.show_formulas {
            theme.text(TextRole::Code)
        } else {
            theme.text(TextRole::Body)
        };
        if numeric && !self.show_formulas {
            style = style.numeric();
        }
        if self.bold.contains(&addr) {
            style = style.strong();
        }
        style
    }

    /// Fit each column to the width of its widest rendered cell so long labels
    /// (e.g. a row title in column A) aren't clipped when the neighbouring cell
    /// is occupied — matching the spreadsheet convention of auto-sizing a
    /// column to its content. A column the user has manually resized keeps its
    /// width; the rest grow to fit, clamped to the default and the autofit
    /// ceiling so they never shrink below the default nor run away for an
    /// extreme value. Measured in the same style the cells are drawn in.
    fn autofit_columns(&mut self, tr: &mut TextRenderer, theme: &Theme) {
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
            let style = self.cell_style(theme, (c, r), val.is_numeric());
            let (tw, _) = tr.measure_style(&text, None, &style);
            let want = f64::from(tw) + self.metrics.cell_pad() * 2.0;
            let entry = widest.entry(c).or_insert(0.0);
            if want > *entry {
                *entry = want;
            }
        }
        let default = self.metrics.default_col_w();
        for (c, want) in widest {
            let fitted = want.clamp(default, self.metrics.max_autofit_w());
            // Only record a widening; narrower columns fall back to the default.
            if fitted > default {
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
            self.autofit_columns(tr, theme);
            self.rebuild_geom();
            self.needs_autofit = false;
        }
        let body = self.body_rect();
        let cols = self.visible_cols();
        let rows = self.visible_rows();

        // The cells stand on the window's own ground; the washes (selection,
        // referenced ranges) go under the text, the outlines over it.
        self.draw_selection_fill(scene, transform, theme, body);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &body);
        self.draw_reference_washes(scene, transform, theme, body);
        self.draw_cells(
            &mut PaintCtx::new(scene, transform, tr, theme),
            &cols,
            &rows,
            body,
        );
        self.draw_grid_lines(scene, transform, theme, &cols, &rows, body);
        self.draw_active_highlight(scene, transform, theme, body);
        self.draw_fill_handle(scene, transform, theme, body);
        self.draw_reference_outlines(scene, transform, theme, body);
        // A column the body's edge cuts fades out there, so a number cut
        // short reads as running on past the edge rather than as a shorter
        // number.
        if cols
            .last()
            .is_some_and(|&c| self.cell_screen_rect(c, 0).x1 > body.x1 + 0.5)
        {
            ui::surface::content_edge_fade(
                scene,
                transform,
                body,
                ui::surface::ContentEdge::Trailing,
                theme.tokens.colors.surfaces.content,
            );
        }
        if rows
            .last()
            .is_some_and(|&r| self.cell_screen_rect(0, r).y1 > body.y1 + 0.5)
        {
            ui::surface::content_edge_fade(
                scene,
                transform,
                body,
                ui::surface::ContentEdge::Bottom,
                theme.tokens.colors.surfaces.content,
            );
        }
        scene.pop_layer();

        // --- headers (painted on top, never clipped by the body) ---
        self.draw_headers(scene, transform, tr, theme, &cols, &rows);

        self.draw_scrollbars(scene, transform, theme);

        // --- in-cell editor overlay ---
        if self.edit.is_some() {
            self.draw_editor(scene, transform, tr, theme);
        }
    }

    /// The library's thumbs, one per overflowing axis; the gutter itself is
    /// bare ground.
    fn draw_scrollbars(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        for axis in [ScrollAxis::Horizontal, ScrollAxis::Vertical] {
            if self.bar_shown(axis) {
                self.bar(axis).draw(
                    scene,
                    transform,
                    self.bar_track(axis),
                    self.bar_metrics(axis),
                    theme,
                    false,
                );
            }
        }
    }

    /// Selection ranges (a lift of the ground) under the cells. Every
    /// discontiguous range paints; overlaps are the same lift.
    fn draw_selection_fill(&self, scene: &mut Scene, transform: Affine, theme: &Theme, body: Rect) {
        let fill = theme.tokens.colors.intent.selection;
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

    /// Paint each visible cell's value/formula text, elided with an ellipsis
    /// where it does not fit the cell or the edge of the body.
    fn draw_cells(&self, ctx: &mut PaintCtx<'_>, cols: &[u32], rows: &[u32], body: Rect) {
        let theme = ctx.theme;
        let colors = &theme.tokens.colors;
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
                // Formula view: show the raw input verbatim, set as code, so
                // it reads as "source" rather than a result.
                if self.show_formulas
                    && let Some(raw) = self.raw.get(&(c, r))
                    && !raw.is_empty()
                {
                    let style = self.cell_style(theme, (c, r), false);
                    self.draw_cell_text(ctx, raw, &style, colors.text_primary, (cell, body), false);
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
                    let style = self.cell_style(theme, (c, r), val.is_numeric());
                    self.draw_cell_text(ctx, &text, &style, color, (cell, body), right_align);
                }
            }
        }
    }

    /// Grid line strokes for the visible rows/columns, plus the trailing
    /// right/bottom edges when they fall within `body`. The line is the
    /// ground's own derived hairline, so it registers on every theme.
    fn draw_grid_lines(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        cols: &[u32],
        rows: &[u32],
        body: Rect,
    ) {
        let line_color = ui::surface::edge_on(theme.tokens.colors.surfaces.content);
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

    /// Active-cell highlight border, in your colour.
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
                theme.tokens.colors.intent.you,
                None,
                &ar,
            );
        }
    }

    /// Fill handle + drag preview. While dragging, a dashed outline shows
    /// the region the pattern will extend into; at rest, a small solid
    /// square sits on the selection's bottom-right corner.
    fn draw_fill_handle(&self, scene: &mut Scene, transform: Affine, theme: &Theme, body: Rect) {
        let colors = &theme.tokens.colors;
        if let Some(fd) = self.fill_drag {
            if let Some((_, _, dtl, dbr)) = Self::fill_region(fd.src, fd.target) {
                let tl = self.cell_screen_rect(dtl.0, dtl.1);
                let br = self.cell_screen_rect(dbr.0, dbr.1);
                if let Some(cr) = intersect(Rect::new(tl.x0, tl.y0, br.x1, br.y1), body) {
                    let dashed = Stroke::new(1.5).with_dashes(0.0, [4.0, 3.0]);
                    scene.stroke(&dashed, transform, colors.intent.you, None, &cr);
                }
            }
        } else if let Some(hr) = self.fill_handle_rect()
            && let Some(cr) = intersect(hr, body)
        {
            // A thin ground-coloured ring keeps the square legible on any
            // cell fill underneath.
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                colors.surfaces.content,
                None,
                &hr,
            );
            scene.fill(Fill::NonZero, transform, colors.intent.you, None, &cr);
        }
    }

    /// The ranges the formula being edited refers to, each with its colour:
    /// the same colour tints the reference in the formula text, so it is
    /// clear which cells feed the result (and, in point mode, which range a
    /// click just inserted).
    fn referenced_ranges(&self, theme: &Theme) -> Vec<(Rect, Color)> {
        let Some(body_text) = self.highlight_formula_body() else {
            return Vec::new();
        };
        let spans = crate::formula::references(&body_text);
        let slots = crate::formula::ref_color_slots(&spans);
        spans
            .iter()
            .zip(&slots)
            .map(|(span, slot)| {
                let tl = self.cell_screen_rect(span.tl.0, span.tl.1);
                let br = self.cell_screen_rect(span.br.0, span.br.1);
                (
                    Rect::new(tl.x0, tl.y0, br.x1, br.y1),
                    reference_color(theme, *slot),
                )
            })
            .collect()
    }

    /// A faint wash of each reference's colour over its cells, under the text.
    fn draw_reference_washes(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        body: Rect,
    ) {
        let colors = &theme.tokens.colors;
        for (range, color) in self.referenced_ranges(theme) {
            if let Some(cr) = intersect(range, body) {
                let wash = colors
                    .tinted_chip_on(
                        colors.surfaces.content,
                        color,
                        0.18,
                        ui::tokens::colors::SMALL_TEXT_MIN_CONTRAST,
                    )
                    .fill;
                scene.fill(Fill::NonZero, transform, wash, None, &cr);
            }
        }
    }

    /// Whether referenced ranges get an outline. Only while a formula is
    /// being edited: on a merely selected formula cell an outline next to the
    /// selection's own reads as a second selected cell, so the trace stays a
    /// wash.
    pub(crate) fn reference_outlines_visible(&self) -> bool {
        self.edit.is_some()
    }

    /// The outline of each referenced range, over the cells.
    fn draw_reference_outlines(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        body: Rect,
    ) {
        if !self.reference_outlines_visible() {
            return;
        }
        for (range, color) in self.referenced_ranges(theme) {
            if let Some(cr) = intersect(range, body) {
                scene.stroke(&Stroke::new(1.5), transform, color, None, &cr);
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

    /// One cell's text, elided with an ellipsis at the cell's edge — or at
    /// the body's, where the cell is cut by it — and centred on its measured
    /// line box so a cell and its row header share one baseline. `place` is
    /// the cell's rect and the body's.
    fn draw_cell_text(
        &self,
        ctx: &mut PaintCtx<'_>,
        text: &str,
        style: &TextStyle,
        color: Color,
        place: (Rect, Rect),
        right_align: bool,
    ) {
        let (cell, body) = place;
        let pad = self.metrics.cell_pad();
        let cut = cell.x1 > body.x1;
        let left = cell.x0 + pad;
        let right = if cut { body.x1 } else { cell.x1 - pad };
        // A right-aligned number stays whole: it is anchored to the cell, and
        // a cell cut by the body shows only what the clip leaves of it.
        let room = if right_align {
            cell.width() - 2.0 * pad
        } else {
            right - left
        };
        if room <= 1.0 {
            return;
        }
        let shown = ctx.text.elide_to_width_style(text, room as f32, style);
        let (tw, th) = ctx.text.measure_style(&shown, None, style);
        let ty = cell.y0 + (cell.height() - f64::from(th)) / 2.0;
        let tx = if right_align {
            cell.x1 - pad - f64::from(tw)
        } else {
            left
        };
        ctx.text(&shown, style, color, Point::new(tx, ty), None);
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
        let corner = Rect::new(self.rect.x0, self.rect.y0, self.grid_x0(), self.grid_y0());
        ui::surface::region(scene, transform, corner, Region::Sidebar, theme);
        self.header_rules(scene, transform, theme);
    }

    /// The headers' two hairlines, on the strips' own ground: under the
    /// column labels and beside the row labels.
    fn header_rules(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let rule = ui::surface::edge_on(theme.region(Region::Sidebar));
        scene.fill(
            Fill::NonZero,
            transform,
            rule,
            None,
            &Rect::new(
                self.rect.x0,
                self.grid_y0() - 1.0,
                self.rect.x1,
                self.grid_y0(),
            ),
        );
        scene.fill(
            Fill::NonZero,
            transform,
            rule,
            None,
            &Rect::new(
                self.grid_x0() - 1.0,
                self.rect.y0,
                self.grid_x0(),
                self.rect.y1,
            ),
        );
    }

    /// Column header strip: letters and the selection lift.
    fn draw_col_headers(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        cols: &[u32],
    ) {
        let colors = &theme.tokens.colors;
        let style = theme.text(TextRole::Caption).numeric();

        let col_strip = Rect::new(self.rect.x0, self.rect.y0, self.rect.x1, self.grid_y0());
        let ground = ui::surface::region(scene, transform, col_strip, Region::Sidebar, theme);
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
                    colors.lift_on(ground, 1.0),
                    None,
                    &hrect,
                );
            }
            let label = crate::model::col_to_letters(c);
            let (lw, lh) = tr.measure_style(&label, None, &style);
            // A column partly scrolled under the row headers is labelled in
            // what shows of it; when not even the label fits, it goes
            // unlabelled rather than painted half under the corner.
            let shown = Rect::new(
                hrect.x0.max(self.grid_x0()),
                hrect.y0,
                hrect.x1.min(self.rect.x1),
                hrect.y1,
            );
            if shown.width() < f64::from(lw) + 2.0 * self.metrics.s1 {
                continue;
            }
            let tx = shown.center().x - f64::from(lw) / 2.0;
            let ty = hrect.y0 + (hrect.height() - f64::from(lh)) / 2.0;
            let color = if selected {
                colors.text_primary
            } else {
                colors.text_dim
            };
            tr.draw_style(
                scene,
                &label,
                color,
                transform * Affine::translate((tx, ty)),
                None,
                &style,
            );
        }
        scene.pop_layer();
    }

    /// Row header strip: numbers and the selection lift.
    fn draw_row_headers(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rows: &[u32],
    ) {
        let colors = &theme.tokens.colors;
        let style = theme.text(TextRole::Caption).numeric();

        let row_strip = Rect::new(self.rect.x0, self.grid_y0(), self.grid_x0(), self.rect.y1);
        let ground = ui::surface::region(scene, transform, row_strip, Region::Sidebar, theme);
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
                    colors.lift_on(ground, 1.0),
                    None,
                    &hrect,
                );
            }
            let label = format!("{}", r + 1);
            let (lw, lh) = tr.measure_style(&label, None, &style);
            // Same rule as the column labels: in what shows of the row.
            let shown_y0 = hrect.y0.max(self.grid_y0());
            let shown_y1 = hrect.y1.min(self.rect.y1);
            if shown_y1 - shown_y0 < f64::from(lh) {
                continue;
            }
            let tx = hrect.center().x - f64::from(lw) / 2.0;
            let ty = (shown_y0 + shown_y1 - f64::from(lh)) / 2.0;
            let color = if selected {
                colors.text_primary
            } else {
                colors.text_dim
            };
            tr.draw_style(
                scene,
                &label,
                color,
                transform * Affine::translate((tx, ty)),
                None,
                &style,
            );
        }
        scene.pop_layer();
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
    fn editor_desired_width(&self, text: &str, tr: &mut TextRenderer, style: &TextStyle) -> f64 {
        let (tw, _) = tr.measure_style(text, None, style);
        // Text width + padding on both sides + caret room, never narrower
        // than the cell itself.
        (f64::from(tw) + self.metrics.cell_pad() * 2.0 + self.metrics.fill_handle())
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

    /// Like Excel: the active edit is a floating field aligned to the cell
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

        let style = theme.text(TextRole::Body);
        let text = e.shown();
        let desired = self.editor_desired_width(&text, tr, &style);

        // Grow rightward across empty neighbours until we have enough room or
        // we hit an occupied cell / the last column / the grid edge.
        let right = self.editor_right_extent((ec, er), desired, body.x1);
        let edit_rect = Rect::new(cell.x0.max(body.x0), cell.y0, right, cell.y1);

        // Clip everything to the body so the floating editor stays inside the
        // grid even when it overflows past where data ends.
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &body);

        // A field: its raised face, so the cells under it do not bleed
        // through, ringed in your colour while it has focus.
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.raised,
            None,
            &edit_rect,
        );
        scene.stroke(
            &Stroke::new(2.0),
            transform,
            colors.intent.you,
            None,
            &edit_rect,
        );

        // Text + caret, clipped to the editor box. A formula draws each cell
        // reference in its own colour; anything else draws in the body colour.
        let (_, line_h) = tr.measure_style("Ag", None, &style);
        let line_h = f64::from(line_h);
        let ty = edit_rect.y0 + (edit_rect.height() - line_h) / 2.0;
        let tx = edit_rect.x0 + self.metrics.cell_pad();
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            transform,
            &edit_rect,
        );
        draw_edit_line(
            scene,
            transform,
            tr,
            theme,
            &style,
            &EditLine {
                text: &text,
                origin: Point::new(tx, ty),
                line_h,
                selection: e.shown_selection(),
                caret: Some(e.shown_caret()),
                preedit: e.shown_preedit(),
            },
        );
        scene.pop_layer();
        scene.pop_layer();
    }
}

/// One line of formula or cell text being edited, for [`draw_edit_line`]:
/// where its line box starts, how tall it is, and the selection and caret in
/// character indices.
pub(crate) struct EditLine<'a> {
    pub text: &'a str,
    pub origin: Point,
    pub line_h: f64,
    pub selection: Option<(usize, usize)>,
    pub caret: Option<usize>,
    pub preedit: Option<(usize, usize)>,
}

/// Paint one editable line in `style`: the selection wash under the text, the
/// text with every cell reference in its own colour, and the caret in your
/// colour. The in-cell editor and the formula bar edit the same buffer, so
/// they paint it through here.
pub(crate) fn draw_edit_line(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    style: &TextStyle,
    line: &EditLine<'_>,
) {
    let colors = &theme.tokens.colors;
    let (tx, ty) = (line.origin.x, line.origin.y);
    if let Some((sel_s, sel_e)) = line.selection {
        let chars: Vec<char> = line.text.chars().collect();
        let sel_s = sel_s.min(chars.len());
        let sel_e = sel_e.min(chars.len());
        let pre: String = chars.get(..sel_s).unwrap_or_default().iter().collect();
        let mid: String = chars.get(sel_s..sel_e).unwrap_or_default().iter().collect();
        let (x0w, _) = tr.measure_style(&pre, None, style);
        let (midw, _) = tr.measure_style(&mid, None, style);
        let sel_rect = Rect::new(
            tx + f64::from(x0w),
            ty,
            tx + f64::from(x0w) + f64::from(midw),
            ty + line.line_h,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            colors.intent.selection_tint,
            None,
            &sel_rect,
        );
    }
    let caret_x = draw_formula_line(
        scene,
        transform,
        tr,
        line.text,
        style,
        theme,
        line.origin,
        line.caret,
    );
    if let Some((start, end)) = line.preedit {
        // The grid cell and formula bar share the exact composed projection.
        // Prefix measurements reuse the renderer cache used by hit mapping.
        let chars: Vec<char> = line.text.chars().collect();
        let prefix: String = chars.iter().take(start).collect();
        let through: String = chars.iter().take(end).collect();
        let x0 = tx + f64::from(tr.measure_style(&prefix, None, style).0);
        let x1 = tx + f64::from(tr.measure_style(&through, None, style).0);
        ui::widget::paint_preedit_underline(
            scene,
            transform,
            Line::new((x0, ty + line.line_h), (x1, ty + line.line_h)),
            theme,
        );
    }
    if line.caret.is_some() {
        scene.stroke(
            &Stroke::new(1.5),
            transform,
            colors.intent.you,
            None,
            &Line::new((caret_x, ty), (caret_x, ty + line.line_h)),
        );
    }
}

/// The colour of the `slot`-th reference in a formula, so each referenced
/// cell/range reads in its own colour both in the formula text and as an
/// outline on the grid — the coloured-reference cue every desktop
/// spreadsheet shows while editing a formula. The presence series is made
/// legible as text on the field the formula is typed in, the lighter of the
/// grounds the colour is drawn on.
pub(crate) fn reference_color(theme: &Theme, slot: usize) -> Color {
    let colors = &theme.tokens.colors;
    colors.ink_on(
        colors.surfaces.raised,
        ChartPalette::from_theme(theme).categorical(slot),
        ON_FILL_MIN_CONTRAST,
    )
}

/// Draw a formula string in `style`, each cell/range reference tinted its
/// assigned colour (the rest in `text_primary`), starting at `origin`
/// (top-left of the text). When `caret` is `Some`, the returned value is the
/// caret's x in scene space so the caller can draw a matching caret;
/// otherwise the return is the x just past the text. Only strings beginning
/// with `=` are scanned for references — a plain value is drawn entirely in
/// `text_primary`.
pub(crate) fn draw_formula_line(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    text: &str,
    style: &TextStyle,
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
            let (pw, _) = tr.measure_style(&prefix, None, style);
            caret_x = x + f64::from(pw);
        }
        let seg: String = chars.get(s..e).unwrap_or_default().iter().collect();
        tr.draw_style(
            scene,
            &seg,
            color,
            transform * Affine::translate((x, origin.y)),
            None,
            style,
        );
        let (sw, _) = tr.measure_style(&seg, None, style);
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
        let other = Theme::light();
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

    #[test]
    fn scrollbars_are_thin_and_only_reserve_space_for_overflow() {
        let mut tr = TextRenderer::new();
        for scale in [1.0, 1.5, 2.0] {
            let theme = Theme::dark().with_ui_scale(scale);
            let mut grid = grid_with(&[]);
            grid.set_metrics(GridMetrics::resolve(&mut tr, &theme));
            grid.rect = Rect::new(0.0, 0.0, 260.0, 180.0);
            let body = grid.body_rect();
            let horizontal = grid.scrollbar_thumb(ScrollAxis::Horizontal).unwrap();
            let vertical = grid.scrollbar_thumb(ScrollAxis::Vertical).unwrap();
            assert!((5.0..=6.0).contains(&horizontal.height()));
            assert!((5.0..=6.0).contains(&vertical.width()));
            assert!(horizontal.y0 >= body.y1 && vertical.x0 >= body.x1);
            grid.rect = Rect::new(
                0.0,
                0.0,
                grid.total_width() + grid.metrics.row_header_w() + 1.0,
                grid.total_height() + grid.metrics.col_header_h() + 1.0,
            );
            assert!(grid.scrollbar_thumb(ScrollAxis::Horizontal).is_none());
            assert!(grid.scrollbar_thumb(ScrollAxis::Vertical).is_none());
            assert_eq!(grid.body_rect().x1, grid.rect.x1);
            assert_eq!(grid.body_rect().y1, grid.rect.y1);
            // Only the vertical axis overflows: no horizontal gutter.
            grid.rect.y1 = 180.0;
            grid.rect.x1 += grid.metrics.gutter;
            assert!(grid.scrollbar_thumb(ScrollAxis::Horizontal).is_none());
            assert!(grid.scrollbar_thumb(ScrollAxis::Vertical).is_some());
            assert_eq!(grid.body_rect().y1, grid.rect.y1);
        }
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
            let control = ControlSize::Small.height(&Theme::dark());
            assert!(
                one_x.default_row_h() >= control && one_x.default_row_h() < control + 2.0,
                "1x row is the small control height or the cell line plus s2"
            );
            assert!(
                (one_x.col_header_h() - control).abs() < 1.0,
                "1x header is the small control height"
            );
            assert!(
                (one_x.row_header_w() - (one_x.s12 + one_x.s2)).abs() < 0.01,
                "1x row header is s12 + s2"
            );

            let mut g = Grid::new();
            g.row_heights.insert(3, 40.0);
            g.rebuild_geom();
            let one_x_top_of_row_2 = g.row_top(2);

            let theme = Theme::dark().with_text_scale(2.0);
            tr.set_text_scale(2.0);
            let two_x = GridMetrics::resolve(&mut tr, &theme);
            // The role scales once, so the measured line box doubled.
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
            let horizontal = grid
                .scrollbar_thumb(ScrollAxis::Horizontal)
                .expect("the wide sheet overflows the narrow viewport");
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

            let vertical = grid
                .scrollbar_thumb(ScrollAxis::Vertical)
                .expect("the tall sheet overflows the short viewport");
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
    /// body starts after the headers and is wide enough for many columns,
    /// with the scroll bar gutters beside it. No scroll.
    fn grid_with(values: &[(Addr, CellValue)]) -> Grid {
        let mut g = Grid::new();
        let m = g.metrics();
        // Body roughly 12 columns wide so overflow has room.
        let width = m.row_header_w() + m.default_col_w() * 12.0 + m.gutter;
        let height = m.col_header_h() + m.default_row_h() * 12.0 + m.gutter;
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
            let theme = Theme::dark();
            let style = g.cell_style(&theme, (0, 0), false);
            let label_w = f64::from(tr.measure_style(label, None, &style).0);
            let want = label_w + g.metrics.cell_pad() * 2.0;
            let default = g.metrics.default_col_w();
            // Precondition: this label really does overflow a default-width column.
            assert!(want > default, "test label must exceed the default width");

            g.autofit_columns(&mut tr, &theme);

            assert!(
                g.col_w(0) + 0.5 >= want,
                "col A width {} must fit the label (needs {want})",
                g.col_w(0),
            );
            assert!(
                g.col_w(0) <= g.metrics.max_autofit_w() + 0.5,
                "auto-fit stays clamped"
            );
            assert_eq!(
                g.col_w(1),
                default,
                "short occupied neighbour is left alone"
            );
        }

        // Case `autofit_leaves_manually_resized_columns_untouched`.
        {
            // A manual resize must win over auto-fit.
            let mut g = grid_with(&[((0, 0), CellValue::Text("Monthly budget total".into()))]);
            g.col_widths.insert(0, 200.0);
            let mut tr = TextRenderer::new();
            g.autofit_columns(&mut tr, &Theme::dark());
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
            let desired = g.metrics.default_col_w() * 3.0;
            let right = g.editor_right_extent((0, 0), desired, body_right);
            // It should reach at least 3 columns wide (within the body).
            assert!(
                right >= cell.x0 + g.metrics.default_col_w() * 3.0 - 0.5,
                "right={right} expected >= {}",
                cell.x0 + g.metrics.default_col_w() * 3.0
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
            let desired = g.metrics.default_col_w() * 6.0; // wants a lot
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
            let right = g.editor_right_extent((0, 0), g.metrics.default_col_w() * 4.0, body_right);
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
            let right =
                g.editor_right_extent((10, 0), g.metrics.default_col_w() * 50.0, body_right);
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
