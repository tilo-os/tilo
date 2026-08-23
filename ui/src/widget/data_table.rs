//! `DataTable` primitive — sortable (and later resizable/virtualized) tabular
//! view used by WorkspaceSettings/Roles, the file explorer list mode, and any
//! shell surface that needs a structured multi-column list.
//!
//! Spec: `docs/superpowers/specs/2026-04-18-nirios-shell-design.md` — Phase A
//! step 11 (`DataTable` primitive + `WorkspaceSettings` modal using it).
//!
//! This first landing (plan task **T4**) implements:
//! - `ColumnWidth` (Fixed / Flex / Auto) and `ColumnDef<Row>` with typed
//!   per-row renderer and a `sortable` flag.
//! - `DataTable<Row>` storing rows + an index `view` (so sort is non-destructive).
//! - `sort_by_with` — typed helper that accepts a comparator and remembers the
//!   last column key + direction so the view is re-applied when rows change.
//! - Minimal header + body rendering using `Theme::tokens` surfaces/border.
//! - Header-click handling that toggles sort direction for `sortable` columns.
//!
//! T5 adds interactive column resizing. T6 adds row virtualization via
//! [`DataTable::set_viewport`]/[`DataTable::set_scroll_y`] and
//! [`DataTable::visible_row_range`], which the `draw` pass honours so only
//! rows within the viewport (plus a small overscan) are painted.

use automerge::{AutoCommit, ObjId};
use std::cmp::Ordering;
use std::collections::HashMap;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;
use crate::widget::scrollbar::{PressOutcome, ScrollMetrics, Scrollbar};

/// Visual thickness of the built-in scrollbar track/thumb, in logical px.
/// Matches [`crate::widget::Scrollable`] so a `DataTable` bar and a
/// wrapping `Scrollable` bar read identically.
const BAR_THICKNESS: f64 = 8.0;
/// Inset of the track from the body edge.
const BAR_MARGIN: f64 = 2.0;

/// How a column contributes to the header/body horizontal layout.
///
/// `Fixed` — literal px width. `Flex(weight)` — share of the remaining space
/// (weighted). `Auto` — sized to content at build time (treated as `Flex(1.0)`
/// until the layout pass can measure it in T5+).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ColumnWidth {
    Fixed(f32),
    Flex(f32),
    Auto,
}

/// Cell renderer with everything needed to paint a cell — the
/// `TextRenderer`, the resolved cell [`Rect`] (already positioned for the
/// row), and whether the row is selected (so the closure can flip the
/// text colour for contrast on the selection fill). Register one per
/// column via [`ColumnDef::with_text_render`]; columns without one render
/// nothing (header + chrome still draw).
pub type CellTextRender<Row> =
    Box<dyn Fn(&Row, &mut Scene, &mut TextRenderer, Rect, &Theme, bool) + Send + Sync>;

/// Type-erased cell renderer. Receives `(&Row, &mut Scene, &Theme, &mut
/// TextRenderer, Rect)` — the row, the scene to paint into, the active
/// theme, the shared text renderer (so a cell can measure + draw real
/// text), and the resolved on-screen cell rectangle (so the closure knows
/// where to anchor its content). Coordinates are absolute scene space;
/// the table-level transform has already been applied.
pub type CellRender<Row> =
    Box<dyn Fn(&Row, &mut Scene, &Theme, &mut TextRenderer, Rect) + Send + Sync>;

/// Definition of a single column.
pub struct ColumnDef<Row> {
    pub key: String,
    pub header: String,
    pub width: ColumnWidth,
    pub sortable: bool,
    /// Per-row cell renderer. `None` renders an empty cell.
    pub text_render: Option<CellTextRender<Row>>,
    /// Always-present per-row cell renderer (no-op by default). Invoked for
    /// every visible cell in the `draw` pass; set via [`ColumnDef::with_render`].
    pub render_cell: CellRender<Row>,
    /// Width in px of the right-edge resize hit area. Used by T5.
    pub(crate) resize_handle_px: f32,
}

impl<Row> ColumnDef<Row> {
    pub fn new(
        key: impl Into<String>,
        header: impl Into<String>,
        width: ColumnWidth,
    ) -> Self {
        Self {
            key: key.into(),
            header: header.into(),
            width,
            sortable: false,
            text_render: None,
            render_cell: Box::new(|_, _, _, _, _| {}),
            resize_handle_px: 6.0,
        }
    }

    pub fn sortable(mut self, v: bool) -> Self {
        self.sortable = v;
        self
    }

    /// Register a text-capable cell renderer.
    pub fn with_text_render(mut self, f: CellTextRender<Row>) -> Self {
        self.text_render = Some(f);
        self
    }

    /// Register the always-present cell renderer, invoked for every visible
    /// cell in `draw` with `(row, scene, theme, text_renderer, rect)`.
    pub fn with_render(mut self, f: CellRender<Row>) -> Self {
        self.render_cell = f;
        self
    }
}

/// Remembered sort state so row changes can re-apply the current ordering.
struct SortState<Row> {
    key: String,
    ascending: bool,
    cmp: Box<dyn Fn(&Row, &Row) -> Ordering + Send + Sync>,
}

/// Active column resize gesture. `origin_x` is the pointer-x at which
/// `begin_resize` was called; `origin_w` is the column width at that moment.
/// Stored so repeated `drag_resize(pointer_x)` calls can compute an absolute
/// new width without drifting.
struct ResizeState {
    key: String,
    origin_x: f32,
    origin_w: f32,
}

pub struct DataTable<Row> {
    columns: Vec<ColumnDef<Row>>,
    rows: Vec<Row>,
    /// Indices into `rows` in display order — sort operates on this so the
    /// original insertion order stays untouched and callers can look up by
    /// original index if needed.
    view: Vec<usize>,
    sort: Option<SortState<Row>>,
    rect: Rect,
    col_rects: Vec<Rect>,
    is_focused: bool,
    /// Resolved per-column pixel widths. Populated lazily from `ColumnWidth`
    /// and then authoritative once the user has dragged a handle.
    col_widths_px: HashMap<String, f32>,
    /// Active drag (pointer captured while `Some`).
    resize: Option<ResizeState>,
    /// Last observed cursor position (for hit-testing press events against the
    /// per-column resize handles).
    last_cursor: Option<Point>,
    /// Virtualization: viewport height in px. `0.0` = unset (fall back to
    /// drawing the full `rect`).
    viewport_h: f32,
    /// Virtualization: row height in px. Defaults to the render-time
    /// [`ROW_HEIGHT`] constant so callers who never call `set_viewport`
    /// still get a sensible `visible_row_range` answer.
    row_height: f32,
    /// Virtualization: current vertical scroll offset in px.
    scroll_y: f32,
    /// When `true`, body rows are selectable: clicks update [`selection`],
    /// rows render a selection fill, and keyboard Up/Down/Enter/Ctrl-A are
    /// honoured while focused. Off by default so existing read-only tables
    /// (e.g. the shell's permission tables) are unaffected.
    selectable: bool,
    /// Display-index selection state (indices into `view`). Seeded by the
    /// owner each frame for stateless hosts; mutated in place on click.
    selection: crate::selection::SelectionModel,
    /// Fired on every body-row press with `(display_index, ctrl, shift)`
    /// so a stateless host (rebuilt each frame) can own the selection.
    on_row_click: Option<Box<dyn FnMut(usize, bool, bool)>>,
    /// Fired when a row is activated (double-click or Enter) with its
    /// display index.
    on_activate: Option<Box<dyn FnMut(usize)>>,
    /// Fired when a sortable header is clicked, with the column index.
    /// When set it *replaces* the internal `toggle_sort` so the host can
    /// own sort order (the file explorer sorts its own model).
    on_header_click: Option<Box<dyn FnMut(usize)>>,
    /// Last body-row press for internal double-click detection (used only
    /// when `on_activate` is set and the table is long-lived).
    last_click_row: Option<(usize, std::time::Instant)>,
    /// Built-in vertical scrollbar interaction model. Active only in
    /// internal-virtualization mode (host called [`set_viewport`] and the
    /// rows overflow that viewport); dormant when the table is wrapped in
    /// an outer [`crate::widget::Scrollable`], so no double bar appears.
    /// Shares the toolkit's scrollbar drag/track logic via
    /// [`crate::widget::scrollbar`].
    vbar: Scrollbar,
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
            columns,
            rows: Vec::new(),
            view: Vec::new(),
            sort: None,
            rect: Rect::ZERO,
            col_rects: Vec::new(),
            is_focused: false,
            col_widths_px,
            resize: None,
            last_cursor: None,
            viewport_h: 0.0,
            row_height: ROW_HEIGHT as f32,
            scroll_y: 0.0,
            selectable: false,
            selection: crate::selection::SelectionModel::new(),
            on_row_click: None,
            on_activate: None,
            on_header_click: None,
            last_click_row: None,
            vbar: Scrollbar::vertical(),
        }
    }

    /// Enable row selection + selection rendering + keyboard navigation.
    pub fn selectable(mut self, v: bool) -> Self {
        self.selectable = v;
        self
    }

    /// Seed the selection (display indices). Stateless hosts call this each
    /// frame from their own [`SelectionModel`](crate::selection::SelectionModel).
    pub fn set_selection(&mut self, sel: crate::selection::SelectionModel) {
        self.selection = sel;
    }

    /// Borrow the current selection.
    pub fn selection(&self) -> &crate::selection::SelectionModel {
        &self.selection
    }

    /// Mutable access to the selection.
    pub fn selection_mut(&mut self) -> &mut crate::selection::SelectionModel {
        &mut self.selection
    }

    /// Builder form of [`set_selection`](DataTable::set_selection).
    pub fn with_selection(mut self, sel: crate::selection::SelectionModel) -> Self {
        self.selection = sel;
        self
    }

    /// Set the per-click handler `(display_index, ctrl, shift)`.
    pub fn on_row_click<F: FnMut(usize, bool, bool) + 'static>(mut self, f: F) -> Self {
        self.on_row_click = Some(Box::new(f));
        self
    }

    /// Set the activation handler (double-click / Enter), given the row's
    /// display index.
    pub fn on_activate<F: FnMut(usize) + 'static>(mut self, f: F) -> Self {
        self.on_activate = Some(Box::new(f));
        self
    }

    /// Set the header-click handler (column index). Replaces the internal
    /// sort toggle so the host can own sort order.
    pub fn on_header_click<F: FnMut(usize) + 'static>(mut self, f: F) -> Self {
        self.on_header_click = Some(Box::new(f));
        self
    }

    /// Map a logical cursor point to a body-row display index, accounting
    /// for the header band and the current scroll offset. Returns `None`
    /// for points in the header or below the last row.
    fn row_at(&self, pt: Point) -> Option<usize> {
        if pt.x < self.rect.x0 || pt.x > self.rect.x1 {
            return None;
        }
        let body_top = self.rect.y0 + HEADER_HEIGHT;
        if pt.y < body_top {
            return None;
        }
        let rel = pt.y - body_top + self.scroll_y as f64;
        if rel < 0.0 {
            return None;
        }
        let idx = (rel / self.row_height as f64) as usize;
        if idx < self.view.len() { Some(idx) } else { None }
    }

    pub fn set_rows(&mut self, rows: Vec<Row>) {
        self.rows = rows;
        self.view = (0..self.rows.len()).collect();
        self.reapply_sort();
    }

    pub fn columns(&self) -> &[ColumnDef<Row>] { &self.columns }

    /// Rows in current sort order.
    pub fn view_rows(&self) -> Vec<&Row> {
        self.view.iter().map(|&i| &self.rows[i]).collect()
    }

    /// Current `(column_key, ascending)` if any sort is active.
    pub fn sort_state(&self) -> Option<(&str, bool)> {
        self.sort.as_ref().map(|s| (s.key.as_str(), s.ascending))
    }

    /// Typed sort helper. Stores the comparator + key + direction so that
    /// `set_rows` re-applies the same ordering automatically.
    pub fn sort_by_with<F>(&mut self, cmp: F, key: &str, ascending: bool)
    where
        F: Fn(&Row, &Row) -> Ordering + Send + Sync + 'static,
    {
        self.sort = Some(SortState {
            key: key.to_string(),
            ascending,
            cmp: Box::new(cmp),
        });
        self.reapply_sort();
    }

    /// Prompt-locked sort entry point. Applies direction `ascending` to the
    /// column `column_key`, **reusing** the comparator previously installed
    /// via [`Self::sort_by_with`]. Returns `true` when the column has a
    /// registered comparator and the sort was applied; `false` when no
    /// comparator is installed for that key (call `sort_by_with` first).
    ///
    /// Rationale: because per-column comparators cannot be encoded as pure
    /// `(key, direction)` without help, callers install typed comparators via
    /// `sort_by_with`, then header-click / keyboard handlers drive subsequent
    /// direction flips through this simpler entry point.
    pub fn sort_by(&mut self, column_key: &str, ascending: bool) -> bool {
        if let Some(s) = &mut self.sort {
            if s.key == column_key {
                s.ascending = ascending;
                self.reapply_sort();
                return true;
            }
        }
        false
    }

    fn reapply_sort(&mut self) {
        if let Some(s) = &self.sort {
            let rows = &self.rows;
            let cmp = &s.cmp;
            let ascending = s.ascending;
            self.view.sort_by(|&a, &b| {
                let o = cmp(&rows[a], &rows[b]);
                if ascending { o } else { o.reverse() }
            });
        }
    }

    /// Resolved pixel width for the column with `key`, if the column exists.
    /// Returns the stored width if one has been observed/resized, otherwise
    /// the width encoded in its `ColumnWidth::Fixed` variant. Flex/Auto
    /// columns return `None` until they've been resized by the user (the
    /// final pixel value for those depends on the layout pass).
    pub fn column_width_px(&self, key: &str) -> Option<f32> {
        if let Some(w) = self.col_widths_px.get(key).copied() {
            return Some(w);
        }
        self.columns.iter().find(|c| c.key == key).and_then(|c| {
            if let ColumnWidth::Fixed(px) = c.width { Some(px) } else { None }
        })
    }

    /// Start a resize gesture on column `key`. `pointer_x` is the pointer's
    /// x-coordinate at the moment of the press; subsequent `drag_resize`
    /// calls compute the new width as `origin_w + (pointer_x - origin_x)`.
    pub fn begin_resize(&mut self, key: &str, pointer_x: f32) {
        let origin_w = self.column_width_px(key).unwrap_or(Self::MIN_COL_WIDTH);
        self.resize = Some(ResizeState {
            key: key.to_string(),
            origin_x: pointer_x,
            origin_w,
        });
    }

    /// Drag the active resize gesture to the given pointer x-coordinate,
    /// clamping to `MIN_COL_WIDTH`.
    pub fn drag_resize(&mut self, pointer_x: f32) {
        if let Some(r) = &self.resize {
            let delta = pointer_x - r.origin_x;
            let new_w = (r.origin_w + delta).max(Self::MIN_COL_WIDTH);
            let key = r.key.clone();
            self.col_widths_px.insert(key, new_w);
        }
    }

    /// End the active resize gesture. No-op if none is active.
    pub fn end_resize(&mut self) {
        self.resize = None;
    }

    /// Virtualization: install the viewport height and per-row height (both
    /// in px). Enables the [`visible_row_range`] windowing used by the draw
    /// pass so only rows that intersect the viewport (plus a small overscan)
    /// are rendered.
    pub fn set_viewport(&mut self, height_px: f32, row_height_px: f32) {
        self.viewport_h = height_px.max(0.0);
        self.row_height = row_height_px.max(1.0);
    }

    /// Virtualization: current vertical scroll offset in px.
    pub fn set_scroll_y(&mut self, y: f32) {
        self.scroll_y = y.max(0.0);
    }

    /// Virtualization: `(start, end)` row-index range (half-open) the draw
    /// pass should render. `start = max(floor(scroll / row_h) - 2, 0)` and
    /// `end = min(start + ceil(vp / row_h) + 4, len)` — a 2-row overscan on
    /// each side keeps scrolling smooth without paying for the full list.
    ///
    /// When no viewport has been set the entire row list is returned so the
    /// fall-back matches the pre-virtualization behaviour.
    pub fn visible_row_range(&self) -> (usize, usize) {
        let len = self.view.len();
        if self.viewport_h <= 0.0 || self.row_height <= 0.0 {
            return (0, len);
        }
        let first = (self.scroll_y / self.row_height).floor() as isize;
        let start = first.saturating_sub(2).max(0) as usize;
        let visible = (self.viewport_h / self.row_height).ceil() as usize;
        let end = (start + visible + 4).min(len);
        (start, end)
    }

    // === Built-in vertical scrollbar =================================
    //
    // Active only in internal-virtualization mode: the host installed a
    // viewport via `set_viewport` *and* the rows overflow it. Otherwise
    // the table draws at full height and an outer `Scrollable` (or the
    // host) owns scrolling — so no bar is drawn and nothing is captured,
    // keeping the change additive.

    /// Height (logical px) of the body band that rows scroll within.
    fn body_viewport_h(&self) -> f64 {
        if self.viewport_h > 0.0 {
            self.viewport_h as f64
        } else {
            (self.rect.height() - HEADER_HEIGHT).max(0.0)
        }
    }

    /// Total height (logical px) of all rows — the scrollable content.
    fn content_body_h(&self) -> f64 {
        self.row_height as f64 * self.view.len() as f64
    }

    /// Furthest the body can scroll (`content - viewport`, floored at 0).
    pub fn max_scroll_y(&self) -> f32 {
        (self.content_body_h() - self.body_viewport_h()).max(0.0) as f32
    }

    /// Scroll geometry along the body's vertical axis, for the shared bar.
    fn v_metrics(&self) -> ScrollMetrics {
        ScrollMetrics::new(self.content_body_h(), self.body_viewport_h(), self.scroll_y as f64)
    }

    /// Whether the built-in bar is live: an explicit viewport was set and
    /// the rows overflow it.
    fn internal_scroll(&self) -> bool {
        self.viewport_h > 0.0 && self.v_metrics().overflows()
    }

    /// Track rect (right edge of the body band) for the built-in bar, or
    /// `None` when it isn't shown.
    fn v_track(&self) -> Option<Rect> {
        if !self.internal_scroll() {
            return None;
        }
        let x1 = self.rect.x1 - BAR_MARGIN;
        let x0 = x1 - BAR_THICKNESS;
        let y0 = self.rect.y0 + HEADER_HEIGHT + BAR_MARGIN;
        let y1 = self.rect.y0 + HEADER_HEIGHT + self.body_viewport_h() - BAR_MARGIN;
        if y1 - y0 <= crate::widget::scrollbar::MIN_THUMB_LEN.min(1.0) {
            return None;
        }
        Some(Rect::new(x0, y0, x1, y1))
    }

    /// The built-in scrollbar thumb rect (screen space), or `None` when the
    /// bar is inactive (no viewport set, or rows fit). Exposed so hosts and
    /// tests can confirm the thumb reflects the current `scroll_y`.
    pub fn scrollbar_thumb(&self) -> Option<Rect> {
        let track = self.v_track()?;
        self.vbar.thumb_rect(track, self.v_metrics())
    }

    /// Paint the built-in scrollbar (track + thumb) when active. Reads bar
    /// colours from the theme so it matches OS chrome and the wrapping
    /// `Scrollable`'s bar.
    fn draw_scrollbar(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let Some(track) = self.v_track() else { return };
        let radius = BAR_THICKNESS / 2.0;
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surface_white_5,
            None,
            &RoundedRect::from_rect(track, radius),
        );
        if let Some(thumb) = self.vbar.thumb_rect(track, self.v_metrics()) {
            let color = if self.vbar.is_active() { theme.text_dim } else { theme.border_standard };
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
        let w = col.resize_handle_px as f64;
        Some(Rect::new(cr.x1 - w, self.rect.y0, cr.x1, self.rect.y0 + HEADER_HEIGHT))
    }

    /// Toggle ordering on the column at `index` if it is sortable. Returns the
    /// new `(key, ascending)` state, or `None` if the column is not sortable
    /// or no comparator has been registered via `sort_by_with` yet.
    ///
    /// Header-click handler calls this — the comparator must have been
    /// installed at least once beforehand (typical usage: the owning widget
    /// installs comparators for every sortable column up-front).
    pub fn toggle_sort(&mut self, index: usize) -> Option<(String, bool)> {
        let col = self.columns.get(index)?;
        if !col.sortable { return None; }
        let key = col.key.clone();
        let ascending = match &self.sort {
            Some(s) if s.key == key => !s.ascending,
            _ => true,
        };
        // Flip direction if we already had a matching sort; otherwise leave
        // the remembered comparator alone — the caller is responsible for
        // installing a comparator for new keys via `sort_by_with`.
        if let Some(s) = &mut self.sort {
            if s.key == key {
                s.ascending = ascending;
                self.reapply_sort();
                return Some((key, ascending));
            }
        }
        None
    }
}

const HEADER_HEIGHT: f64 = 28.0;
const ROW_HEIGHT: f64 = 24.0;
const CELL_PADDING_X: f64 = 8.0;

impl<Row: 'static> Widget for DataTable<Row> {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let height = HEADER_HEIGHT + ROW_HEIGHT * self.view.len().max(1) as f64;
        let node = taffy
            .new_leaf(Style {
                // Fill the parent's width (a bare `auto` leaf with no
                // measure collapses to zero under compute_layout_with_measure).
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(height as f32),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
        // Resolve per-column x-ranges across the header/body. User-resized
        // widths in `col_widths_px` override the declared `ColumnWidth`.
        self.col_rects = compute_column_rects(&self.columns, self.rect, &self.col_widths_px);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let tokens = &theme.tokens;
        // Body surface.
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            tokens.colors.surfaces.base,
            None,
            &self.rect,
        );

        // Header strip (`surface_raised` + subtle bottom border).
        let header_rect = Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x1,
            self.rect.y0 + HEADER_HEIGHT,
        );
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            tokens.colors.surfaces.raised,
            None,
            &header_rect,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            tokens.colors.border_standard,
            None,
            &vello::kurbo::Line::new(
                (header_rect.x0, header_rect.y1),
                (header_rect.x1, header_rect.y1),
            ),
        );

        // Column headers.
        let text_size = tokens.typography.scale.sm;
        for (i, col) in self.columns.iter().enumerate() {
            let Some(cr) = self.col_rects.get(i) else { continue };
            let label_y =
                header_rect.y0 + (HEADER_HEIGHT - text_size as f64) / 2.0;
            text_renderer.draw_text(
                scene,
                &col.header,
                text_size,
                tokens.colors.text_primary,
                transform
                    * Affine::translate((cr.x0 + CELL_PADDING_X, label_y)),
                None,
            );
            // Sort indicator arrow (▲/▼) if this column is the active sort.
            if let Some(s) = &self.sort {
                if s.key == col.key {
                    let arrow = if s.ascending { "\u{25B2}" } else { "\u{25BC}" };
                    text_renderer.draw_text(
                        scene,
                        arrow,
                        text_size,
                        tokens.colors.text_dim,
                        transform
                            * Affine::translate((cr.x1 - 16.0, label_y)),
                        None,
                    );
                }
            }
        }

        // Body rows — virtualization windows the loop to the visible range
        // plus a small overscan (see `visible_row_range`).
        let (vis_start, vis_end) = self.visible_row_range();
        let row_h = self.row_height as f64;
        for ri in vis_start..vis_end {
            let Some(&row_idx) = self.view.get(ri) else { break };
            // Subtract the scroll offset so internal virtualization places
            // the visible window at the top of the body band. When the table
            // is driven by an outer Scrollable, scroll_y stays 0 and the
            // whole rect is translated instead — both paths render correctly.
            let y0 = self.rect.y0 + HEADER_HEIGHT + (ri as f64) * row_h - self.scroll_y as f64;
            let y1 = y0 + row_h;
            if y1 <= self.rect.y0 + HEADER_HEIGHT { continue; }
            if y0 >= self.rect.y1 { break; }
            let selected = self.selectable && self.selection.is_selected(ri);
            let row_rect = Rect::new(self.rect.x0, y0, self.rect.x1, y1.min(self.rect.y1));
            // Selection fill takes precedence over the zebra stripe.
            if selected {
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    tokens.colors.accent.stops[crate::tokens::colors::ACCENT_PRIMARY_IDX],
                    None,
                    &row_rect,
                );
            } else if ri % 2 == 1 {
                // Alternate row striping using the neutral ramp.
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    tokens.colors.neutral[5],
                    None,
                    &row_rect,
                );
            }
            // Per-column cell render. Both renderers receive the resolved
            // cell rect (column x-range × this row's y-band, clamped to the
            // table's bottom edge), painting in absolute scene coordinates.
            // `render_cell` is the always-present closure (no-op by default)
            // set via `with_render`; `text_render` is the optional
            // text-capable closure (set via `with_text_render`) that also
            // gets the `selected` flag so it can flip the text colour for
            // contrast on the selection fill.
            for (i, col) in self.columns.iter().enumerate() {
                let cell_rect = match self.col_rects.get(i) {
                    Some(cr) => Rect::new(cr.x0, y0, cr.x1, y1.min(self.rect.y1)),
                    None => continue,
                };
                (col.render_cell)(
                    &self.rows[row_idx],
                    scene,
                    theme,
                    text_renderer,
                    cell_rect,
                );
                if let Some(tr) = &col.text_render {
                    tr(&self.rows[row_idx], scene, text_renderer, cell_rect, theme, selected);
                }
            }
        }

        // Keyboard-focus ring around the whole table, so arrow-key row
        // navigation reads as a focused control and not just a striped grid.
        if self.is_focused {
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                theme.border_focus,
                None,
                &self.rect,
            );
        }

        // Built-in scrollbar composites on top of the body (only in
        // internal-virtualization mode; otherwise a no-op).
        self.draw_scrollbar(scene, transform, theme);
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let pt = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_cursor = Some(pt);
                if self.resize.is_some() {
                    self.drag_resize(pt.x as f32);
                    *handled = true;
                    return;
                }
                // An active scrollbar drag owns the move — map it to a new
                // scroll offset and swallow the event.
                if self.vbar.is_dragging() {
                    if let Some(track) = self.v_track() {
                        if let Some(target) = self.vbar.drag_to(track, self.v_metrics(), pt) {
                            self.set_scroll_y(target as f32);
                        }
                    }
                    *handled = true;
                    return;
                }
                // Otherwise update the thumb hover brightening.
                if let Some(track) = self.v_track() {
                    self.vbar.update_hover(track, self.v_metrics(), pt);
                } else {
                    self.vbar.set_hover(false);
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let Some(pt) = self.last_cursor else { return };
                // 0. The built-in scrollbar wins over the body beneath it:
                //    thumb press → drag; track press → page toward the click.
                if let Some(track) = self.v_track() {
                    match self.vbar.press(track, self.v_metrics(), pt) {
                        PressOutcome::Thumb => {
                            *handled = true;
                            return;
                        }
                        PressOutcome::Page(target) => {
                            self.set_scroll_y(target as f32);
                            *handled = true;
                            return;
                        }
                        PressOutcome::Miss => {}
                    }
                }
                // 1. Column resize handles win over everything.
                for i in 0..self.columns.len() {
                    if let Some(handle) = self.resize_handle_rect_for(i) {
                        if handle.contains(pt) {
                            let key = self.columns[i].key.clone();
                            self.begin_resize(&key, pt.x as f32);
                            *handled = true;
                            return;
                        }
                    }
                }
                // 2. Header click → sort (host-owned via on_header_click,
                //    else internal toggle).
                let in_header = pt.y >= self.rect.y0
                    && pt.y < self.rect.y0 + HEADER_HEIGHT
                    && pt.x >= self.rect.x0
                    && pt.x <= self.rect.x1;
                if in_header {
                    if let Some(i) = self.col_rects.iter().position(|c| pt.x >= c.x0 && pt.x < c.x1) {
                        if self.columns.get(i).map(|c| c.sortable).unwrap_or(false) {
                            if let Some(cb) = &mut self.on_header_click {
                                cb(i);
                            } else {
                                self.toggle_sort(i);
                            }
                            *handled = true;
                        }
                    }
                    return;
                }
                // 3. Body-row selection.
                if self.selectable {
                    if let Some(ri) = self.row_at(pt) {
                        let ctrl = modifiers.state().control_key() || modifiers.state().super_key();
                        let shift = modifiers.state().shift_key();
                        self.selection.apply_click(ri, ctrl, shift);
                        *focus_target = Some(node_id);
                        if let Some(cb) = &mut self.on_row_click {
                            cb(ri, ctrl, shift);
                        }
                        // Internal double-click → activate (long-lived hosts).
                        let now = std::time::Instant::now();
                        let is_double = matches!(
                            self.last_click_row,
                            Some((pri, t)) if pri == ri && now.duration_since(t) < std::time::Duration::from_millis(500)
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
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                let mut did = false;
                if self.resize.is_some() {
                    self.end_resize();
                    did = true;
                }
                if self.vbar.release() {
                    did = true;
                }
                if did {
                    *handled = true;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // Internal-virtualization mode scrolls the body on the
                // wheel. When the table is wrapped in a `Scrollable`, the
                // wheel is consumed by the parent before it reaches here,
                // so this only fires for a self-scrolling table.
                if self.internal_scroll() {
                    const LINE_PX: f32 = 60.0;
                    let dy = match delta {
                        MouseScrollDelta::LineDelta(_, y) => *y * -LINE_PX,
                        MouseScrollDelta::PixelDelta(pp) => -pp.y as f32,
                    };
                    let next = (self.scroll_y + dy).clamp(0.0, self.max_scroll_y());
                    if next != self.scroll_y {
                        self.set_scroll_y(next);
                    }
                    *handled = true;
                }
            }
            WindowEvent::KeyboardInput { event: ke, .. }
                if self.selectable && self.is_focused && ke.state == ElementState::Pressed =>
            {
                use winit::keyboard::{Key, NamedKey};
                let shift = modifiers.state().shift_key();
                let len = self.view.len();
                if len == 0 {
                    return;
                }
                let cur = self.selection.primary();
                match &ke.logical_key {
                    Key::Named(NamedKey::ArrowDown) => {
                        let next = cur.map(|c| (c + 1).min(len - 1)).unwrap_or(0);
                        self.selection.move_to(next, shift);
                        *handled = true;
                    }
                    Key::Named(NamedKey::ArrowUp) => {
                        let next = cur.map(|c| c.saturating_sub(1)).unwrap_or(0);
                        self.selection.move_to(next, shift);
                        *handled = true;
                    }
                    Key::Named(NamedKey::Enter) => {
                        if let Some(c) = cur {
                            if let Some(cb) = &mut self.on_activate {
                                cb(c);
                            }
                        }
                        *handled = true;
                    }
                    Key::Character(s) if (s == "a" || s == "A") && modifiers.state().control_key() => {
                        self.selection.select_all(len);
                        *handled = true;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn wants_pointer_capture(&self) -> bool { self.resize.is_some() || self.vbar.is_dragging() }

    fn name(&self) -> &str { "DataTable" }
    fn test_id(&self) -> Option<&str> { Some("data_table") }
    fn get_rect(&self) -> Rect { self.rect }
    fn is_focusable(&self) -> bool { true }
    fn set_focus(&mut self, focused: bool) { self.is_focused = focused; }
    fn is_focused(&self) -> bool { self.is_focused }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        // Selection is by display index (indices into `view`). Surface the
        // primary selected row's 1-based position as structured position info so
        // the reader renders a consistent "N of M".
        let selected_pos = self.selection.primary();
        // Describe the active sort using the column's human header (not its
        // internal key) so the announcement reads naturally.
        let description = self.sort_state().map(|(key, ascending)| {
            let header = self
                .columns
                .iter()
                .find(|c| c.key == key)
                .map(|c| c.header.as_str())
                .unwrap_or(key);
            let dir = if ascending { "ascending" } else { "descending" };
            format!("sorted by {header} {dir}")
        });
        let mut node = AccessNode::new(Role::Table)
            .with_state(AccessState {
                focusable: self.is_focusable(),
                focused: self.is_focused(),
                ..Default::default()
            });
        if let Some(i) = selected_pos {
            node = node.with_position(i + 1, self.view.len());
        }
        if let Some(desc) = description {
            node = node.with_description(desc);
        }
        // Expose the grid structure: each display row is a `Row` carrying its
        // 1-based position, with one `Cell` per column. The cell content comes
        // from an opaque closure renderer, so a cell's accessible name is its
        // column header — enough for an AT to say "row 2, Name column" and
        // pair it with the text the reader reads off the screen.
        let row_count = self.view.len();
        let col_count = self.columns.len();
        // The keyboard cursor (Up/Down move it, Enter activates it) is the
        // table's *active descendant*: when the table holds keyboard focus, the
        // export layer puts the reader on that row rather than on the table as a
        // whole, so arrowing down reads each row instead of repeating "table".
        let cursor = if self.selectable { selected_pos } else { None };
        let rows: Vec<AccessNode> = (0..row_count)
            .map(|ri| {
                let selected = self.selectable && self.selection.is_selected(ri);
                let cells: Vec<AccessNode> = self
                    .columns
                    .iter()
                    .enumerate()
                    .map(|(ci, col)| {
                        AccessNode::new(Role::Cell)
                            .with_name(col.header.clone())
                            .with_position(ci + 1, col_count)
                    })
                    .collect();
                AccessNode::new(Role::Row)
                    .with_position(ri + 1, row_count)
                    .with_state(AccessState {
                        selected,
                        // Rows are addressed by position, so they carry no
                        // explicit key: their ordinal *is* the identity the user
                        // means, and it stays correct across a re-sort.
                        focusable: self.selectable,
                        focused: cursor == Some(ri),
                        ..Default::default()
                    })
                    .with_children(cells)
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
        state: &mut automerge::AutoCommit,
        path: &automerge::ObjId,
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
        let row = row_key as usize;
        // An id an AT held across a change can name a row that no longer
        // exists. Drop it — acting on whatever now sits at that index would
        // operate on the wrong record.
        if row >= self.view.len() {
            return false;
        }
        match action {
            // The host moves keyboard focus to the table; we move our own
            // cursor to the requested row, which is the half only we can do.
            Action::Focus => {
                self.selection.move_to(row, false);
                true
            }
            Action::Activate | Action::ToggleCheck => {
                self.selection.move_to(row, false);
                if let Some(cb) = &mut self.on_activate {
                    cb(row);
                }
                true
            }
            _ => false,
        }
    }
}

fn compute_column_rects<Row>(
    columns: &[ColumnDef<Row>],
    host: Rect,
    overrides: &HashMap<String, f32>,
) -> Vec<Rect> {
    let total_w = host.width() as f32;
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
                    if flex_sum <= 0.0 { 0.0 } else { flex_remaining * (w.max(0.0) / flex_sum) }
                }
                ColumnWidth::Auto => {
                    if flex_sum <= 0.0 { 0.0 } else { flex_remaining * (1.0 / flex_sum) }
                }
            }
        };
        let next = x + w as f64;
        rects.push(Rect::new(x, y0, next, y1));
        x = next;
    }
    rects
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct R { name: String }

    fn sample_cols() -> Vec<ColumnDef<R>> {
        vec![
            ColumnDef::new("name", "Name", ColumnWidth::Flex(1.0)).sortable(true),
            ColumnDef::new("x", "X", ColumnWidth::Fixed(40.0)),
        ]
    }

    #[test]
    fn compute_column_rects_splits_flex_after_fixed() {
        let cols = sample_cols();
        let host = Rect::new(0.0, 0.0, 200.0, 28.0);
        let rects = compute_column_rects(&cols, host, &HashMap::new());
        assert_eq!(rects.len(), 2);
        assert!((rects[0].width() - 160.0).abs() < 0.01);
        assert!((rects[1].width() - 40.0).abs() < 0.01);
    }

    #[test]
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

    #[test]
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

    #[test]
    fn data_table_test_id_returns_data_table() {
        let t = DataTable::<R>::new(sample_cols());
        assert_eq!(Widget::test_id(&t), Some("data_table"));
    }

    #[test]
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
        assert_eq!(node.description.as_deref(), Some("sorted by Name ascending"));
    }

    #[test]
    fn accessibility_no_sort_or_selection_omits_value_and_description() {
        let t = DataTable::<R>::new(sample_cols());
        let node = Widget::accessibility(&t).unwrap();
        assert_eq!(node.value, None, "no selection → no value");
        assert_eq!(node.pos_in_set, None, "no selection → no position");
        assert_eq!(node.set_size, None, "no selection → no set size");
        assert_eq!(node.description, None, "no active sort → no active sort description");
    }

    #[test]
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
        assert!(node.children[2].state.selected, "row 3 (index 2) is selected");
        assert!(!node.children[0].state.selected);
    }

    // --- Selection / interaction tests --------------------------------

    use std::cell::RefCell;
    use std::rc::Rc;
    use winit::event::{DeviceId, ElementState, MouseButton};

    /// Lay a table out inside a 300×400 flex-column root so its auto
    /// width stretches to 300 and rows get real hit rects, then return
    /// the taffy tree + the table's node for event dispatch.
    fn lay_out(table: &mut DataTable<R>) -> (TaffyTree<()>, NodeId) {
        let mut tree = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let theme = Theme::dark();
        let tnode = table.build_layout(&mut tree, &theme, &mut node_map);
        let root = tree
            .new_with_children(
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
            )
            .unwrap();
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

    fn dispatch(
        t: &mut DataTable<R>,
        tree: &TaffyTree<()>,
        node: NodeId,
        ev: WindowEvent,
        mods: &winit::event::Modifiers,
    ) {
        let mut handled = false;
        let mut focus = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        t.handle_event(&ev, 1.0, mods, &mut doc, &root, &mut handled, &mut focus, node, tree);
    }

    fn device() -> DeviceId {
        unsafe { std::mem::zeroed() }
    }

    #[test]
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

        // Row 2 spans y ∈ [28 + 2*24, +24) = [76, 100). Click at (50, 85).
        dispatch(&mut t, &tree, node, WindowEvent::CursorMoved { device_id: device(), position: (50.0, 85.0).into() }, &mods);
        dispatch(&mut t, &tree, node, WindowEvent::MouseInput { device_id: device(), state: ElementState::Pressed, button: MouseButton::Left }, &mods);

        assert!(t.selection().is_selected(2), "row 2 selected");
        assert_eq!(t.selection().primary(), Some(2));
        assert_eq!(*clicks.borrow(), vec![(2, false, false)]);
    }

    #[test]
    fn header_click_fires_on_header_click_instead_of_toggle() {
        let hits = Rc::new(RefCell::new(Vec::<usize>::new()));
        let mut t = {
            let hits = hits.clone();
            DataTable::<R>::new(sample_cols()).on_header_click(move |i| hits.borrow_mut().push(i))
        };
        t.set_rows(rows4());
        // Install a comparator so a fallback toggle *would* have sorted —
        // proving the callback path takes precedence (rows stay put).
        t.sort_by_with(|a, b| a.name.cmp(&b.name), "name", true);
        let (tree, node) = lay_out(&mut t);
        let mods = winit::event::Modifiers::default();

        // Header band is y ∈ [0, 28). Name column starts at x=0. Click (20, 10).
        dispatch(&mut t, &tree, node, WindowEvent::CursorMoved { device_id: device(), position: (20.0, 10.0).into() }, &mods);
        dispatch(&mut t, &tree, node, WindowEvent::MouseInput { device_id: device(), state: ElementState::Pressed, button: MouseButton::Left }, &mods);

        assert_eq!(*hits.borrow(), vec![0], "header click reported column 0");
        // Sort direction unchanged because the callback replaced toggle.
        assert_eq!(t.sort_state(), Some(("name", true)));
    }

    #[test]
    fn non_selectable_table_ignores_body_clicks() {
        let mut t = DataTable::<R>::new(sample_cols()); // selectable defaults false
        t.set_rows(rows4());
        let (tree, node) = lay_out(&mut t);
        let mods = winit::event::Modifiers::default();
        dispatch(&mut t, &tree, node, WindowEvent::CursorMoved { device_id: device(), position: (50.0, 85.0).into() }, &mods);
        dispatch(&mut t, &tree, node, WindowEvent::MouseInput { device_id: device(), state: ElementState::Pressed, button: MouseButton::Left }, &mods);
        assert!(t.selection().is_empty(), "read-only table keeps empty selection");
    }

    #[test]
    fn keyboard_arrows_move_selection_when_focused() {
        let mut t = DataTable::<R>::new(sample_cols()).selectable(true);
        t.set_rows(rows4());
        t.set_focus(true);
        let (tree, node) = lay_out(&mut t);
        let mods = winit::event::Modifiers::default();
        // winit keeps `KeyEvent`'s extra field `pub(crate)`; the suite-wide
        // workaround is to zero-init and set the fields we care about.
        let key = |named| {
            let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
            ev.logical_key = winit::keyboard::Key::Named(named);
            ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
                winit::keyboard::NativeKeyCode::Unidentified,
            );
            ev.location = winit::keyboard::KeyLocation::Standard;
            ev.state = ElementState::Pressed;
            ev.repeat = false;
            WindowEvent::KeyboardInput { device_id: device(), event: ev, is_synthetic: false }
        };
        use winit::keyboard::NamedKey;
        dispatch(&mut t, &tree, node, key(NamedKey::ArrowDown), &mods); // → 0
        dispatch(&mut t, &tree, node, key(NamedKey::ArrowDown), &mods); // → 1
        assert_eq!(t.selection().primary(), Some(1));
        assert!(t.selection().is_selected(1));
    }

    #[test]
    fn internal_scroll_offsets_hit_testing() {
        // Regression for the virtualization bug: when scrolled via the
        // table's own scroll_y, a click at the top of the body band must
        // resolve to the first *scrolled-in* row, matching where draw now
        // positions it (draw subtracts scroll_y; row_at adds it).
        let mut t = DataTable::<R>::new(sample_cols()).selectable(true);
        t.set_rows((0..100).map(|i| R { name: format!("r{i}") }).collect());
        let _laid = lay_out(&mut t);
        t.set_viewport(5.0 * ROW_HEIGHT as f32, ROW_HEIGHT as f32);
        t.set_scroll_y(10.0 * ROW_HEIGHT as f32); // scrolled past 10 rows
        let rect = t.get_rect();
        let p = Point::new(rect.x0 + 5.0, rect.y0 + HEADER_HEIGHT + 1.0);
        assert_eq!(t.row_at(p), Some(10), "top of body maps to the scrolled-in row");
        // visible window also starts around row 10 (minus overscan).
        let (start, _end) = t.visible_row_range();
        assert!(start <= 10 && start >= 8, "window starts near the scroll position: {start}");
    }

    // --- Built-in scrollbar tests -------------------------------------

    /// A 100-row table laid out at 300×400 then put into internal-scroll
    /// mode with a 5-row viewport (body viewport 120 px over 2400 px of
    /// rows → the bar is live). X of the bar centre is 294.
    fn scrolling_table() -> (DataTable<R>, TaffyTree<()>, NodeId) {
        let mut t = DataTable::<R>::new(sample_cols()).selectable(true);
        t.set_rows((0..100).map(|i| R { name: format!("r{i}") }).collect());
        let (tree, node) = lay_out(&mut t);
        t.set_viewport(5.0 * ROW_HEIGHT as f32, ROW_HEIGHT as f32);
        (t, tree, node)
    }

    fn bar_x(t: &DataTable<R>) -> f64 {
        t.get_rect().x1 - BAR_MARGIN - BAR_THICKNESS / 2.0
    }

    #[test]
    fn scrollbar_hidden_without_viewport() {
        // No `set_viewport` → the table draws full-height and defers to an
        // outer Scrollable; no built-in bar (avoids a double bar).
        let mut t = DataTable::<R>::new(sample_cols());
        t.set_rows((0..100).map(|i| R { name: format!("r{i}") }).collect());
        let _ = lay_out(&mut t);
        assert!(t.scrollbar_thumb().is_none(), "no bar without an explicit viewport");
    }

    #[test]
    fn scrollbar_hidden_when_rows_fit() {
        let mut t = DataTable::<R>::new(sample_cols());
        t.set_rows(rows4());
        let _ = lay_out(&mut t);
        // Viewport taller than the 4 rows → content fits → no bar.
        t.set_viewport(50.0 * ROW_HEIGHT as f32, ROW_HEIGHT as f32);
        assert!(t.scrollbar_thumb().is_none(), "no bar when rows fit the viewport");
    }

    #[test]
    fn scrollbar_thumb_tracks_scroll_y() {
        let (mut t, _tree, _node) = scrolling_table();
        let track = t.v_track().expect("bar live");
        let top = t.scrollbar_thumb().expect("thumb at top");
        assert!((top.y0 - track.y0).abs() < 1e-6, "thumb starts at track top");

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

    #[test]
    fn dragging_scrollbar_updates_scroll_y() {
        let (mut t, tree, node) = scrolling_table();
        let thumb = t.scrollbar_thumb().unwrap();
        let x = bar_x(&t);
        let mid_y = (thumb.y0 + thumb.y1) / 2.0;
        let mods = winit::event::Modifiers::default();

        dispatch(&mut t, &tree, node, WindowEvent::CursorMoved { device_id: device(), position: (x, mid_y).into() }, &mods);
        dispatch(&mut t, &tree, node, WindowEvent::MouseInput { device_id: device(), state: ElementState::Pressed, button: MouseButton::Left }, &mods);
        assert!(t.vbar.is_dragging(), "press on thumb begins a drag");
        assert!(t.wants_pointer_capture(), "drag captures the pointer");
        assert_eq!(t.scroll_y, 0.0);

        // Drag down 50 px → scroll_y increases, staying clamped to max.
        dispatch(&mut t, &tree, node, WindowEvent::CursorMoved { device_id: device(), position: (x, mid_y + 50.0).into() }, &mods);
        assert!(t.scroll_y > 0.0, "drag down increases scroll_y: {}", t.scroll_y);
        assert!(t.scroll_y <= t.max_scroll_y(), "clamped to max");
        let after = t.scroll_y;

        // Drag back up past the start → scroll_y returns toward 0.
        dispatch(&mut t, &tree, node, WindowEvent::CursorMoved { device_id: device(), position: (x, mid_y - 50.0).into() }, &mods);
        assert!(t.scroll_y < after, "drag up decreases scroll_y");

        dispatch(&mut t, &tree, node, WindowEvent::MouseInput { device_id: device(), state: ElementState::Released, button: MouseButton::Left }, &mods);
        assert!(!t.vbar.is_dragging(), "release ends the drag");
    }

    #[test]
    fn track_click_pages_scroll_y() {
        let (mut t, tree, node) = scrolling_table();
        let thumb = t.scrollbar_thumb().unwrap();
        let x = bar_x(&t);
        let below_y = thumb.y1 + 5.0;
        let mods = winit::event::Modifiers::default();

        dispatch(&mut t, &tree, node, WindowEvent::CursorMoved { device_id: device(), position: (x, below_y).into() }, &mods);
        dispatch(&mut t, &tree, node, WindowEvent::MouseInput { device_id: device(), state: ElementState::Pressed, button: MouseButton::Left }, &mods);
        assert!(!t.vbar.is_dragging(), "track click is not a drag");
        // Paged by ≈ one viewport (120 px) × PAGE_FRACTION.
        let expected = 120.0 * crate::widget::scrollbar::PAGE_FRACTION as f32;
        assert!(
            (t.scroll_y - expected).abs() < 1.0,
            "page step ≈ viewport*frac ({expected}), got {}",
            t.scroll_y,
        );
    }

    #[test]
    fn wheel_scrolls_in_internal_mode() {
        let (mut t, tree, node) = scrolling_table();
        let mods = winit::event::Modifiers::default();
        assert_eq!(t.scroll_y, 0.0);
        let wheel = WindowEvent::MouseWheel {
            device_id: device(),
            delta: MouseScrollDelta::LineDelta(0.0, -1.0),
            phase: winit::event::TouchPhase::Moved,
        };
        dispatch(&mut t, &tree, node, wheel, &mods);
        assert!(t.scroll_y > 0.0, "wheel down increases scroll_y: {}", t.scroll_y);
        assert!(t.scroll_y <= t.max_scroll_y(), "clamped to max");
    }

    #[test]
    fn scrollbar_press_does_not_select_a_row() {
        // A press on the bar must be swallowed before body-row selection.
        let (mut t, tree, node) = scrolling_table();
        let thumb = t.scrollbar_thumb().unwrap();
        let x = bar_x(&t);
        let mid_y = (thumb.y0 + thumb.y1) / 2.0;
        let mods = winit::event::Modifiers::default();
        dispatch(&mut t, &tree, node, WindowEvent::CursorMoved { device_id: device(), position: (x, mid_y).into() }, &mods);
        dispatch(&mut t, &tree, node, WindowEvent::MouseInput { device_id: device(), state: ElementState::Pressed, button: MouseButton::Left }, &mods);
        assert!(t.selection().is_empty(), "scrollbar press must not select a row");
    }

    fn table_color_fills(t: &mut DataTable<R>) -> usize {
        use vello::Scene;
        use vello_encoding::DrawTag;
        let _laid = lay_out(t);
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        t.draw(&mut scene, vello::kurbo::Affine::IDENTITY, &mut tr, &theme, &presence);
        scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|tag| **tag == DrawTag::COLOR)
            .count()
    }

    /// A focused table paints a focus ring around its rect — one extra stroke
    /// over the unfocused render. Asserts on the production `draw` path.
    #[test]
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
}
