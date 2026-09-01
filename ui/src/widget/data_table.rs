//! `DataTable` primitive — sortable, resizable, virtualized tabular view.
//!
//! Sort is non-destructive: rows stay in insertion order and an index `view`
//! carries the display order, so [`DataTable::sort_by_with`] can re-apply the
//! remembered comparator whenever rows change. Row virtualization
//! ([`DataTable::set_viewport`]/[`DataTable::visible_row_range`]) windows the
//! `draw` pass to the visible rows plus a small overscan.

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
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::scrollbar::{PressOutcome, ScrollMetrics, Scrollbar};

/// Matches [`crate::widget::Scrollable`] so a `DataTable` bar and a
/// wrapping `Scrollable` bar read identically.
const BAR_THICKNESS: f64 = 8.0;
const BAR_MARGIN: f64 = 2.0;

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

pub struct ColumnDef<Row> {
    pub key: String,
    pub header: String,
    pub width: ColumnWidth,
    pub sortable: bool,
    /// `None` renders an empty cell.
    pub text_render: Option<CellTextRender<Row>>,
    /// No-op by default; set via [`ColumnDef::with_render`].
    pub render_cell: CellRender<Row>,
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
            resize_handle_px: 6.0,
        }
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

    #[must_use]
    pub fn with_render(mut self, f: CellRender<Row>) -> Self {
        self.render_cell = f;
        self
    }
}

pub type SortCmp<Row> = Box<dyn Fn(&Row, &Row) -> Ordering + Send + Sync>;

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

pub struct DataTable<Row> {
    columns: Vec<ColumnDef<Row>>,
    rows: Vec<Row>,
    /// Indices into `rows` in display order — sort operates on this so the
    /// original insertion order stays untouched.
    view: Vec<usize>,
    sort: Option<SortState<Row>>,
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
    /// `0.0` = unset (fall back to drawing the full `rect`).
    viewport_h: f32,
    /// Defaults to [`ROW_HEIGHT`] so callers who never call `set_viewport`
    /// still get a sensible `visible_row_range` answer.
    row_height: f32,
    scroll_y: f32,
    /// Clicks update [`selection`], rows render a selection fill, and
    /// keyboard Up/Down/Enter/Ctrl-A are honoured while focused. Off by
    /// default so existing read-only tables are unaffected.
    selectable: bool,
    /// Indices into `view`. Seeded by the owner each frame for stateless
    /// hosts; mutated in place on click.
    selection: crate::selection::SelectionModel,
    /// `(display_index, ctrl, shift)`, so a stateless host (rebuilt each
    /// frame) can own the selection.
    on_row_click: Option<RowClick>,
    on_activate: Option<Box<dyn FnMut(usize)>>,
    /// When set, *replaces* the internal `toggle_sort` so the host can own
    /// sort order (the file explorer sorts its own model).
    on_header_click: Option<Box<dyn FnMut(usize)>>,
    /// For internal double-click detection (used only when `on_activate` is
    /// set and the table is long-lived).
    last_click_row: Option<(usize, std::time::Instant)>,
    /// Active only in internal-virtualization mode (host called
    /// `set_viewport` and rows overflow it); dormant when wrapped in an
    /// outer [`crate::widget::Scrollable`], to avoid a double bar.
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
            row_height: px(ROW_HEIGHT),
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

    /// `None` for points in the header or below the last row.
    fn row_at(&self, pt: Point) -> Option<usize> {
        if pt.x < self.rect.x0 || pt.x > self.rect.x1 {
            return None;
        }
        let body_top = self.rect.y0 + HEADER_HEIGHT;
        if pt.y < body_top {
            return None;
        }
        let rel = pt.y - body_top + f64::from(self.scroll_y);
        if rel < 0.0 {
            return None;
        }
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "rel >= 0.0 is checked above and row_height is always > 0 \
                      (default ROW_HEIGHT or set_viewport's max(1.0) clamp); \
                      this cast is the intended floor-to-row-index conversion"
        )]
        let idx = (rel / f64::from(self.row_height)) as usize;
        if idx < self.view.len() {
            Some(idx)
        } else {
            None
        }
    }

    pub fn set_rows(&mut self, rows: Vec<Row>) {
        self.rows = rows;
        self.view = (0..self.rows.len()).collect();
        self.reapply_sort();
    }

    pub fn columns(&self) -> &[ColumnDef<Row>] {
        &self.columns
    }

    /// Rows in current sort order.
    pub fn view_rows(&self) -> Vec<&Row> {
        self.view.iter().filter_map(|&i| self.rows.get(i)).collect()
    }

    pub fn sort_state(&self) -> Option<(&str, bool)> {
        self.sort.as_ref().map(|s| (s.key.as_str(), s.ascending))
    }

    /// Stores the comparator + key + direction so `set_rows` re-applies the
    /// same ordering automatically.
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

    /// Reuses the comparator already installed via [`Self::sort_by_with`];
    /// returns `false` when none is installed for `column_key`. Per-column
    /// comparators can't be encoded as pure `(key, direction)`, so callers
    /// install one typed comparator, then header-click/keyboard handlers flip
    /// direction through this simpler entry point.
    pub fn sort_by(&mut self, column_key: &str, ascending: bool) -> bool {
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
        }
    }

    pub fn end_resize(&mut self) {
        self.resize = None;
    }

    /// Enables the [`visible_row_range`] windowing so only rows intersecting
    /// the viewport (plus overscan) are rendered.
    pub fn set_viewport(&mut self, height_px: f32, row_height_px: f32) {
        self.viewport_h = height_px.max(0.0);
        self.row_height = row_height_px.max(1.0);
    }

    pub fn set_scroll_y(&mut self, y: f32) {
        self.scroll_y = y.max(0.0);
    }

    /// `(start, end)` row-index range (half-open), with a 2-row overscan on
    /// each side so scrolling stays smooth. With no viewport set, the entire
    /// row list is returned.
    pub fn visible_row_range(&self) -> (usize, usize) {
        let len = self.view.len();
        if self.viewport_h <= 0.0 || self.row_height <= 0.0 {
            return (0, len);
        }
        // scroll_y and row_height are both non-negative (enforced by
        // set_scroll_y/set_viewport), so these floored/ceiled quotients are
        // already integral and >= 0 — the casts below only narrow that
        // already-integral value, bounded by the on-screen row count.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "scroll_y/row_height >= 0 is enforced by set_scroll_y/set_viewport; \
                      floor()/ceil() already yields an integral, non-negative value"
        )]
        let (first, visible) = (
            (self.scroll_y / self.row_height).floor() as usize,
            (self.viewport_h / self.row_height).ceil() as usize,
        );
        let start = first.saturating_sub(2);
        let end = start.saturating_add(visible).saturating_add(4).min(len);
        (start, end)
    }

    fn body_viewport_h(&self) -> f64 {
        if self.viewport_h > 0.0 {
            f64::from(self.viewport_h)
        } else {
            (self.rect.height() - HEADER_HEIGHT).max(0.0)
        }
    }

    fn content_body_h(&self) -> f64 {
        f64::from(self.row_height) * count_f64(self.view.len())
    }

    /// `content - viewport`, floored at 0.
    pub fn max_scroll_y(&self) -> f32 {
        px((self.content_body_h() - self.body_viewport_h()).max(0.0))
    }

    fn v_metrics(&self) -> ScrollMetrics {
        ScrollMetrics::new(
            self.content_body_h(),
            self.body_viewport_h(),
            f64::from(self.scroll_y),
        )
    }

    /// Live when an explicit viewport was set and the rows overflow it.
    fn internal_scroll(&self) -> bool {
        self.viewport_h > 0.0 && self.v_metrics().overflows()
    }

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
            theme.tokens.colors.surface_white_5,
            None,
            &RoundedRect::from_rect(track, radius),
        );
        if let Some(thumb) = self.vbar.thumb_rect(track, self.v_metrics()) {
            let color = if self.vbar.is_active() {
                theme.text_dim
            } else {
                theme.border_standard
            };
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
            self.rect.y0 + HEADER_HEIGHT,
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

        let text_size = tokens.typography.scale.sm;
        for (i, col) in self.columns.iter().enumerate() {
            let Some(cr) = self.col_rects.get(i) else {
                continue;
            };
            let label_y = header_rect.y0 + (HEADER_HEIGHT - f64::from(text_size)) / 2.0;
            text_renderer.draw_text(
                scene,
                &col.header,
                text_size,
                tokens.colors.text_primary,
                translated(transform, cr.x0 + CELL_PADDING_X, label_y),
                None,
            );
            if let Some(s) = &self.sort
                && s.key == col.key
            {
                let arrow = if s.ascending { "\u{25B2}" } else { "\u{25BC}" };
                text_renderer.draw_text(
                    scene,
                    arrow,
                    text_size,
                    tokens.colors.text_dim,
                    translated(transform, cr.x1 - 16.0, label_y),
                    None,
                );
            }
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
        let row_h = f64::from(self.row_height);
        for ri in vis_start..vis_end {
            let Some(&row_idx) = self.view.get(ri) else {
                break;
            };
            let Some(row) = self.rows.get(row_idx) else {
                continue;
            };
            // Internal virtualization subtracts scroll_y; when driven by an
            // outer Scrollable, scroll_y stays 0 and the whole rect is
            // translated instead — both paths render correctly.
            let y0 =
                self.rect.y0 + HEADER_HEIGHT + count_f64(ri) * row_h - f64::from(self.scroll_y);
            let y1 = y0 + row_h;
            if y1 <= self.rect.y0 + HEADER_HEIGHT {
                continue;
            }
            if y0 >= self.rect.y1 {
                break;
            }
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
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    tokens.colors.neutral[5],
                    None,
                    &row_rect,
                );
            }
            for (i, col) in self.columns.iter().enumerate() {
                let cell_rect = match self.col_rects.get(i) {
                    Some(cr) => Rect::new(cr.x0, y0, cr.x1, y1.min(self.rect.y1)),
                    None => continue,
                };
                (col.render_cell)(row, scene, theme, text_renderer, cell_rect);
                if let Some(tr) = &col.text_render {
                    tr(row, scene, text_renderer, cell_rect, theme, selected);
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
            && pt.y < self.rect.y0 + HEADER_HEIGHT
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
        // 3. Body-row selection.
        if self.selectable
            && let Some(ri) = self.row_at(pt)
        {
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
    fn on_left_release(&mut self, handled: &mut bool) {
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

    /// `WindowEvent::MouseWheel` — only live when this table owns its scroll
    /// internally (an outer `Scrollable` otherwise consumes the wheel first).
    fn on_wheel(&mut self, delta: &MouseScrollDelta, handled: &mut bool) {
        if self.internal_scroll() {
            const LINE_PX: f32 = 60.0;
            let dy = match delta {
                MouseScrollDelta::LineDelta(_, y) => *y * -LINE_PX,
                MouseScrollDelta::PixelDelta(pp) => px(-pp.y),
            };
            let next = (self.scroll_y + dy).clamp(0.0, self.max_scroll_y());
            self.set_scroll_y(next);
            *handled = true;
        }
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
        let len = self.view.len();
        if len == 0 {
            return;
        }
        let cur = self.selection.primary();
        match &ke.logical_key {
            Key::Named(NamedKey::ArrowDown) => {
                let next = cur.map_or(0, |c| c.saturating_add(1).min(len.saturating_sub(1)));
                self.selection.move_to(next, shift);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowUp) => {
                let next = cur.map_or(0, |c| c.saturating_sub(1));
                self.selection.move_to(next, shift);
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
                self.selection.select_all(len);
                *handled = true;
            }
            _ => {}
        }
    }
}

const HEADER_HEIGHT: f64 = 28.0;
const ROW_HEIGHT: f64 = 24.0;
const CELL_PADDING_X: f64 = 8.0;

/// Widens a row/column count to `f64` for layout math, through `u32` (exact
/// in `f64`) with a saturating fallback rather than an `as` cast — a
/// `DataTable` never approaches `u32::MAX` rows or columns.
fn count_f64(n: usize) -> f64 {
    f64::from(u32::try_from(n).unwrap_or(u32::MAX))
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

impl<Row: 'static> Widget for DataTable<Row> {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let height = HEADER_HEIGHT + ROW_HEIGHT * count_f64(self.view.len().max(1));
        let node = taffy.leaf_node(Style {
            // A bare `auto` leaf with no measure collapses to zero
            // under compute_layout_with_measure.
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(px(height)),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
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
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &self.rect,
        );

        self.draw_header(scene, transform, text_renderer, theme);
        self.draw_rows(scene, transform, text_renderer, theme);

        // Focus ring, so arrow-key navigation reads as a focused control.
        if self.is_focused {
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                theme.border_focus,
                None,
                &self.rect,
            );
        }

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
                self.on_cursor_moved(pt, handled);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.on_left_press(*modifiers, handled, focus_target, node_id);
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                self.on_left_release(handled);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.on_wheel(delta, handled);
            }
            WindowEvent::KeyboardInput { event: ke, .. }
                if self.selectable && self.is_focused && ke.state == ElementState::Pressed =>
            {
                self.on_key(ke, *modifiers, handled);
            }
            _ => {}
        }
    }

    fn wants_pointer_capture(&self) -> bool {
        self.resize.is_some() || self.vbar.is_dragging()
    }

    fn name(&self) -> &'static str {
        "DataTable"
    }
    fn test_id(&self) -> Option<&str> {
        Some("data_table")
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
        let selected_pos = self.selection.primary();
        // Human header, not the internal key, so the announcement reads naturally.
        let description = self.sort_state().map(|(key, ascending)| {
            let header = self
                .columns
                .iter()
                .find(|c| c.key == key)
                .map_or(key, |c| c.header.as_str());
            let dir = if ascending { "ascending" } else { "descending" };
            format!("sorted by {header} {dir}")
        });
        let mut node = AccessNode::new(Role::Table).with_state(AccessState {
            focusable: self.is_focusable(),
            focused: self.is_focused(),
            ..Default::default()
        });
        if let Some(i) = selected_pos {
            node = node.with_position(i.saturating_add(1), self.view.len());
        }
        if let Some(desc) = description {
            node = node.with_description(desc);
        }
        // Cell content comes from an opaque closure renderer, so a cell's
        // accessible name is its column header instead.
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
                            .with_position(ci.saturating_add(1), col_count)
                    })
                    .collect();
                AccessNode::new(Role::Row)
                    .with_position(ri.saturating_add(1), row_count)
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
        let Ok(row) = usize::try_from(row_key) else {
            return false;
        };
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(
            node.description.as_deref(),
            Some("sorted by Name ascending")
        );
    }

    #[test]
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

    #[test]
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

    #[test]
    fn a_read_only_tables_rows_are_not_focusable() {
        // Nothing to operate, so nothing should advertise itself as operable.
        let mut t = DataTable::<R>::new(sample_cols());
        t.set_rows(rows4());
        assert!(!t.selectable, "the default table is read-only");
        let node = Widget::accessibility(&t).unwrap();
        assert!(node.children.iter().all(|r| !r.state.focusable));
        assert!(node.children.iter().all(|r| !r.state.focused));
    }

    #[test]
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
        assert!(
            node.children[2].state.selected,
            "row 3 (index 2) is selected"
        );
        assert!(!node.children[0].state.selected);
    }

    use std::cell::RefCell;
    use std::rc::Rc;
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
            1.0,
            &mods,
            &mut doc,
            &root,
            &mut handled,
            &mut focus,
            node,
            tree,
        );
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

    #[test]
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

    #[test]
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

    #[test]
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
        let p = Point::new(rect.x0 + 5.0, rect.y0 + HEADER_HEIGHT + 1.0);
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

    #[test]
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

    #[test]
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

    #[test]
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
        assert_eq!(t.scroll_y, 0.0);

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
            t.scroll_y > 0.0,
            "drag down increases scroll_y: {}",
            t.scroll_y
        );
        assert!(t.scroll_y <= t.max_scroll_y(), "clamped to max");
        let after = t.scroll_y;

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
        assert!(t.scroll_y < after, "drag up decreases scroll_y");

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

    #[test]
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
        // Paged by ≈ one viewport (120 px) × PAGE_FRACTION.
        let expected = 120.0 * px(crate::widget::scrollbar::PAGE_FRACTION);
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
        dispatch(&mut t, &tree, node, &wheel, mods);
        assert!(
            t.scroll_y > 0.0,
            "wheel down increases scroll_y: {}",
            t.scroll_y
        );
        assert!(t.scroll_y <= t.max_scroll_y(), "clamped to max");
    }

    #[test]
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

    /// One extra stroke over the unfocused render, asserted on the production `draw` path.
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
