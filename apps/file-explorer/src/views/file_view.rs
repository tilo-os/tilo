//! `FileView` — the file explorer's main content pane, in either **list** or
//! **grid** mode. A single self-contained widget so the two modes share one
//! selection / hit-test / hover / zoom implementation and one clean, modern
//! visual language:
//!
//! * **no zebra striping** — rows are transparent; a selected row gets a solid
//!   accent highlight and a hovered row a barely-there veneer, exactly like
//!   GNOME Files / Finder;
//! * **comfortable, zoomable metrics** — row height, icon size and grid cell
//!   size all scale from one `scale` factor the app drives from Ctrl +/-,
//!   Ctrl-scroll;
//! * a **real responsive grid** that flows left-to-right and wraps to fill the
//!   available width (the leaf fills its parent's width, so the column count is
//!   derived from the true pane width, never collapsing to a single column);
//! * single-click selects, double-click activates (the app owns the
//!   double-click timing and navigates into folders / opens files).
//!
//! Like the file explorer's other children it is rebuilt each frame by a
//! stateless host: seed the selection + zoom, react to the pushed
//! [`FileCommand`]s. Ephemeral per-frame state (hover) is recomputed from the
//! cursor, so nothing is lost across a rebuild.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::command::{push, CommandSink, FileCommand};
use crate::model::{format_modified, human_size, Entry, SortBy, SortOrder, SyncState, ViewMode};
use ui::icons::{draw_icon, icon_for_path, Icon, LUCIDE_DEFAULT_STROKE_PX};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use ui::SelectionModel;

// --- base metrics (multiplied by `scale`) ---------------------------------

const LIST_ROW_H: f64 = 34.0;
const LIST_HEADER_H: f64 = 30.0;
const LIST_ICON: f64 = 18.0;
const CONTENT_PAD_X: f64 = 10.0;
const ROW_RADIUS: f64 = 8.0;
/// Fixed (pre-scale) widths of the trailing list columns.
const SIZE_COL_W: f64 = 96.0;
const MOD_COL_W: f64 = 158.0;
const COL_GAP: f64 = 10.0;

const GRID_CELL_W: f64 = 112.0;
const GRID_CELL_H: f64 = 104.0;
const GRID_ICON: f64 = 46.0;
const GRID_GAP: f64 = 16.0;

/// A list column the header exposes for click-to-sort. `sort_col` is the index
/// the app's `SortColumn` command understands (1 = name, 2 = size, 3 = date).
struct ColSpec {
    header: &'static str,
    sort_col: usize,
    sort_by: SortBy,
}

const LIST_COLUMNS: [ColSpec; 3] = [
    ColSpec { header: "Name", sort_col: 1, sort_by: SortBy::Name },
    ColSpec { header: "Size", sort_col: 2, sort_by: SortBy::Size },
    ColSpec { header: "Modified", sort_col: 3, sort_by: SortBy::Modified },
];

pub struct FileView {
    entries: Vec<Entry>,
    selection: SelectionModel,
    mode: ViewMode,
    scale: f64,
    sort_by: SortBy,
    sort_order: SortOrder,
    sink: CommandSink,

    rect: Rect,
    /// Grid columns resolved at the last layout (always `>= 1`).
    columns: usize,
    hover: Option<usize>,
    last_cursor: Point,
    is_focused: bool,
    /// When `Some(i)`, item `i` is being renamed in place: its label is omitted
    /// and the label's on-screen rect is published into `rename_rect` so the
    /// host can draw the shared rename editor exactly where the name was.
    renaming: Option<usize>,
    rename_rect: Option<Rc<Cell<Option<Rect>>>>,
    /// The scroll viewport observed at the last `draw`, so the header
    /// hit-test can follow the pinned header (which sits at the viewport top
    /// once the list is scrolled). `None` before the first paint.
    last_vp: Option<Rect>,
    /// A plain press on a directory row `(press point, its path)`, held until
    /// the pointer either releases (a click) or travels past the drag threshold
    /// (begins a drag-to-pin toward the sidebar).
    pressed_dir: Option<(Point, std::path::PathBuf)>,
}

/// Pointer travel (logical px) past which a held directory press becomes a
/// drag rather than a click.
const DRAG_THRESHOLD: f64 = 6.0;

impl FileView {
    pub fn new(entries: &[Entry], selection: &SelectionModel, sink: &CommandSink) -> Self {
        Self {
            entries: entries.to_vec(),
            selection: selection.clone(),
            mode: ViewMode::List,
            scale: 1.0,
            sort_by: SortBy::Name,
            sort_order: SortOrder::Ascending,
            sink: sink.clone(),
            rect: Rect::ZERO,
            columns: 1,
            hover: None,
            last_cursor: Point::ZERO,
            is_focused: false,
            last_vp: None,
            renaming: None,
            rename_rect: None,
            pressed_dir: None,
        }
    }

    pub fn mode(mut self, mode: ViewMode) -> Self {
        self.mode = mode;
        self
    }

    /// Render item `index` (if any) as an in-place rename: omit its label and
    /// publish the label's on-screen rect into `sink` so the host can draw the
    /// shared rename editor over it.
    pub fn with_rename(mut self, index: Option<usize>, sink: Rc<Cell<Option<Rect>>>) -> Self {
        self.renaming = index;
        self.rename_rect = Some(sink);
        self
    }
    pub fn with_scale(mut self, scale: f64) -> Self {
        self.scale = scale.clamp(0.5, 3.0);
        self
    }
    pub fn with_sort(mut self, by: SortBy, order: SortOrder) -> Self {
        self.sort_by = by;
        self.sort_order = order;
        self
    }

    // --- scaled metrics ---------------------------------------------------

    fn row_h(&self) -> f64 { LIST_ROW_H * self.scale }
    fn header_h(&self) -> f64 { LIST_HEADER_H * self.scale }
    fn cell_w(&self) -> f64 { GRID_CELL_W * self.scale }
    fn cell_h(&self) -> f64 { GRID_CELL_H * self.scale }
    fn grid_gap(&self) -> f64 { GRID_GAP * self.scale }
    fn pad_x(&self) -> f64 { CONTENT_PAD_X * self.scale }
    fn body_text(&self, theme: &Theme) -> f32 {
        (theme.tokens.typography.scale.base as f64 * self.scale) as f32
    }
    fn header_text(&self, theme: &Theme) -> f32 {
        theme.tokens.typography.scale.sm
    }

    /// Grid columns that fit `width` (never zero).
    fn columns_for_width(&self, width: f64) -> usize {
        let inner = (width - 2.0 * self.pad_x()).max(0.0);
        let stride = self.cell_w() + self.grid_gap();
        if stride <= 0.0 {
            return 1;
        }
        (((inner + self.grid_gap()) / stride).floor() as usize).max(1)
    }

    fn grid_rows(&self, columns: usize) -> usize {
        if self.entries.is_empty() { 0 } else { self.entries.len().div_ceil(columns.max(1)) }
    }

    fn content_height(&self) -> f64 {
        match self.mode {
            ViewMode::List => self.header_h() + self.entries.len() as f64 * self.row_h() + 6.0,
            ViewMode::Grid => {
                let cols = self.columns.max(1);
                let rows = self.grid_rows(cols);
                self.pad_x() + rows as f64 * (self.cell_h() + self.grid_gap())
            }
        }
    }

    /// Top-left of grid cell `i` within `self.rect`.
    fn cell_rect(&self, i: usize) -> Rect {
        let cols = self.columns.max(1);
        let col = i % cols;
        let row = i / cols;
        let x = self.rect.x0 + self.pad_x() + col as f64 * (self.cell_w() + self.grid_gap());
        let y = self.rect.y0 + self.pad_x() + row as f64 * (self.cell_h() + self.grid_gap());
        Rect::new(x, y, x + self.cell_w(), y + self.cell_h())
    }

    fn list_row_rect(&self, i: usize) -> Rect {
        let y0 = self.rect.y0 + self.header_h() + i as f64 * self.row_h();
        Rect::new(self.rect.x0, y0, self.rect.x1, y0 + self.row_h())
    }

    /// The item under a logical point, or `None` (header band, gaps, empties).
    fn item_at(&self, pt: Point) -> Option<usize> {
        if pt.x < self.rect.x0 || pt.x > self.rect.x1 || pt.y < self.rect.y0 {
            return None;
        }
        match self.mode {
            ViewMode::List => {
                let body_top = self.rect.y0 + self.header_h();
                if pt.y < body_top {
                    return None;
                }
                // Reject clicks that land on the pinned header band (which
                // floats over the scrolled-away top rows) so a click there
                // hits the header, not the row hidden beneath it.
                if pt.y < self.header_top(self.last_vp) + self.header_h() {
                    return None;
                }
                let idx = ((pt.y - body_top) / self.row_h()) as usize;
                (idx < self.entries.len()).then_some(idx)
            }
            ViewMode::Grid => {
                let cols = self.columns.max(1);
                let rel_x = pt.x - self.rect.x0 - self.pad_x();
                let rel_y = pt.y - self.rect.y0 - self.pad_x();
                if rel_x < 0.0 || rel_y < 0.0 {
                    return None;
                }
                let sx = self.cell_w() + self.grid_gap();
                let sy = self.cell_h() + self.grid_gap();
                let col = (rel_x / sx) as usize;
                let row = (rel_y / sy) as usize;
                if col >= cols {
                    return None;
                }
                // Reject the inter-cell gaps.
                if rel_x - col as f64 * sx > self.cell_w() || rel_y - row as f64 * sy > self.cell_h() {
                    return None;
                }
                let idx = row * cols + col;
                (idx < self.entries.len()).then_some(idx)
            }
        }
    }

    /// Responsive column visibility: hide the Modified column on a narrow pane
    /// and the Size column on a very narrow one, so the Name always has room
    /// (like GNOME Files / Finder collapsing detail columns).
    fn show_size(&self) -> bool {
        self.rect.width() > 340.0
    }
    fn show_modified(&self) -> bool {
        self.rect.width() > 560.0
    }
    /// Whether list column `i` (0 name, 1 size, 2 modified) is currently shown.
    fn col_visible(&self, i: usize) -> bool {
        match i {
            0 => true,
            1 => self.show_size(),
            _ => self.show_modified(),
        }
    }

    /// The `SortColumn` command index for a header click at `pt`, if any.
    /// The header band follows the pinned header position, so header clicks
    /// keep sorting even after the list is scrolled.
    fn header_sort_at(&self, pt: Point) -> Option<usize> {
        if self.mode != ViewMode::List {
            return None;
        }
        let top = self.header_top(self.last_vp);
        if pt.y < top || pt.y >= top + self.header_h() {
            return None;
        }
        for (i, col) in LIST_COLUMNS.iter().enumerate() {
            if self.col_visible(i) {
                let cr = self.header_col_rect(i);
                if pt.x >= cr.x0 && pt.x < cr.x1 {
                    return Some(col.sort_col);
                }
            }
        }
        None
    }

    /// x-range of list column `i` (0 = name/flex, 1 = size, 2 = modified),
    /// accounting for which trailing columns are currently visible.
    fn header_col_rect(&self, i: usize) -> Rect {
        let x0 = self.rect.x0 + self.pad_x();
        let x1 = self.rect.x1 - self.pad_x();
        let size_w = SIZE_COL_W * self.scale;
        let mod_w = MOD_COL_W * self.scale;
        let gap = COL_GAP * self.scale;
        let show_mod = self.show_modified();
        let show_size = self.show_size();
        let mod_x0 = if show_mod { x1 - mod_w } else { x1 };
        let size_right = if show_mod { mod_x0 - gap } else { x1 };
        let size_x0 = size_right - size_w;
        let name_x1 = if show_size {
            size_x0 - gap
        } else if show_mod {
            mod_x0 - gap
        } else {
            x1
        };
        let top = self.rect.y0;
        let bot = self.rect.y0 + self.header_h();
        match i {
            0 => Rect::new(x0, top, name_x1, bot),
            1 => Rect::new(size_x0, top, size_x0 + size_w, bot),
            _ => Rect::new(mod_x0, top, mod_x0 + mod_w, bot),
        }
    }

    fn cell_text(theme: &Theme, selected: bool) -> Color {
        if selected { Color::WHITE } else { theme.tokens.colors.text_primary }
    }
    fn cell_dim(theme: &Theme, selected: bool) -> Color {
        if selected {
            Color::from_rgba8(255, 255, 255, 0xd0)
        } else {
            theme.tokens.colors.text_dim
        }
    }

    // --- draw -------------------------------------------------------------

    /// The y-coordinate the column header paints at. It sits at the top of the
    /// content while at rest, but once the list is scrolled it pins to the
    /// viewport's top edge (`vp.y0`) so the Name/Size/Modified labels + the
    /// active sort caret stay visible while the rows scroll beneath — the
    /// GNOME Files / Finder sticky-header behaviour.
    fn header_top(&self, vp: Option<Rect>) -> f64 {
        match vp {
            Some(v) if self.rect.y0 < v.y0 => v.y0,
            _ => self.rect.y0,
        }
    }

    /// Paint the (possibly pinned) column header: an opaque band so rows scroll
    /// cleanly beneath it, the Name/Size/Modified labels, the active sort
    /// caret, and the hairline separator. Drawn *after* the rows so it always
    /// composites on top.
    fn draw_list_header(&self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme, vp: Option<Rect>) {
        let tokens = &theme.tokens;
        let header_h = self.header_h();
        let hsize = self.header_text(theme);
        let top = self.header_top(vp);
        // Opaque band matching the content background so scrolled rows never
        // bleed through the (transparent-text) header.
        let band = Rect::new(self.rect.x0, top, self.rect.x1, top + header_h);
        scene.fill(Fill::NonZero, transform, tokens.colors.surfaces.base, None, &band);
        // Column labels + a sort caret on the active column. Hidden (collapsed)
        // columns are skipped.
        for (i, col) in LIST_COLUMNS.iter().enumerate() {
            if !self.col_visible(i) {
                continue;
            }
            let cr = self.header_col_rect(i);
            let ty = top + (header_h - hsize as f64) / 2.0;
            tr.draw_text(
                scene,
                col.header,
                hsize,
                tokens.colors.text_dim,
                transform * Affine::translate((cr.x0, ty)),
                None,
            );
            if self.sort_by == col.sort_by {
                let caret = if self.sort_order == SortOrder::Ascending { "\u{25B4}" } else { "\u{25BE}" };
                let (lw, _) = tr.measure(col.header, hsize, None);
                tr.draw_text(
                    scene,
                    caret,
                    hsize,
                    tokens.colors.accent.stops[ui::tokens::colors::ACCENT_PRIMARY_IDX],
                    transform * Affine::translate((cr.x0 + lw as f64 + 4.0, ty)),
                    None,
                );
            }
        }
        // Quiet header separator along the band's bottom edge.
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            tokens.colors.border_white_10,
            None,
            &Line::new(
                (self.rect.x0 + self.pad_x(), top + header_h),
                (self.rect.x1 - self.pad_x(), top + header_h),
            ),
        );
    }

    fn draw_list(&self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme, vp: Option<Rect>) {
        let tokens = &theme.tokens;
        let size = self.body_text(theme);
        let icon_px = LIST_ICON * self.scale;
        for (i, entry) in self.entries.iter().enumerate() {
            let row = self.list_row_rect(i);
            if cull(vp, row) {
                continue;
            }
            let selected = self.selection.is_selected(i);
            let hovered = self.hover == Some(i) && !selected;
            // Row highlight — inset so it reads as a pill with breathing room.
            if selected || hovered {
                let hl = Rect::new(row.x0 + 6.0, row.y0 + 2.0, row.x1 - 6.0, row.y1 - 2.0);
                let bg = if selected {
                    tokens.colors.accent.stops[ui::tokens::colors::ACCENT_PRIMARY_IDX]
                } else {
                    tokens.colors.surface_white_10
                };
                scene.fill(Fill::NonZero, transform, bg, None, &RoundedRect::from_rect(hl, ROW_RADIUS));
            }

            // Icon + name.
            let name_rect = self.header_col_rect(0);
            let icon_x = name_rect.x0 + 4.0;
            let icon_y = row.y0 + (row.height() - icon_px) / 2.0;
            let icon_color = if selected {
                Color::WHITE
            } else if entry.is_dir {
                tokens.colors.accent.stops[ui::tokens::colors::ACCENT_PRIMARY_IDX]
            } else {
                tokens.colors.text_dim
            };
            let file_icon_rect = Rect::new(icon_x, icon_y, icon_x + icon_px, icon_y + icon_px);
            draw_icon(
                scene,
                transform,
                file_icon_rect,
                icon_for_path(&entry.path, entry.is_dir),
                icon_color,
                LUCIDE_DEFAULT_STROKE_PX * self.scale.max(1.0),
            );
            // Sync-state emblem for workspace files (cloud / downloading / local).
            if let Some(state) = entry.sync_state {
                draw_sync_badge(scene, transform, file_icon_rect, state, self.scale, theme);
            }
            // Optically center the row text so it lines up with the icon, which is
            // centered by its box (see `crate::text_layout`).
            let text_y = crate::text_layout::centered_text_y(row.y0, row.height(), size);
            let name_x = icon_x + icon_px + 10.0;
            if self.renaming == Some(i) {
                // Publish the name field's rect and omit the label; the host
                // draws the in-place rename editor here.
                if let Some(sink) = &self.rename_rect {
                    sink.set(Some(Rect::new(
                        name_x - 4.0,
                        row.y0 + 3.0,
                        name_rect.x1 - 4.0,
                        row.y1 - 3.0,
                    )));
                }
            } else {
                let label = if entry.is_conflict() {
                    format!("{}  \u{26A0}", entry.name)
                } else {
                    entry.name.clone()
                };
                let avail = (name_rect.x1 - name_x - 6.0).max(0.0);
                let label = truncate(tr, &label, size, avail);
                tr.draw_text(
                    scene,
                    &label,
                    size,
                    Self::cell_text(theme, selected),
                    transform * Affine::translate((name_x, text_y)),
                    None,
                );
            }
            // Size.
            if self.show_size() {
                let size_rect = self.header_col_rect(1);
                tr.draw_text(
                    scene,
                    &human_size(entry),
                    size,
                    Self::cell_dim(theme, selected),
                    transform * Affine::translate((size_rect.x0, text_y)),
                    None,
                );
            }
            // Modified.
            if self.show_modified() {
                let mod_rect = self.header_col_rect(2);
                tr.draw_text(
                    scene,
                    &format_modified(entry.modified),
                    size,
                    Self::cell_dim(theme, selected),
                    transform * Affine::translate((mod_rect.x0, text_y)),
                    None,
                );
            }
        }
        // Header last so it composites above the rows (and stays put while
        // they scroll beneath the pinned band).
        self.draw_list_header(scene, transform, tr, theme, vp);
    }

    fn draw_grid(&self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme, vp: Option<Rect>) {
        let tokens = &theme.tokens;
        let size = self.body_text(theme);
        let icon_px = GRID_ICON * self.scale;
        for (i, entry) in self.entries.iter().enumerate() {
            let cell = self.cell_rect(i);
            if cull(vp, cell) {
                continue;
            }
            let selected = self.selection.is_selected(i);
            let hovered = self.hover == Some(i) && !selected;
            if selected || hovered {
                let bg = if selected {
                    tokens.colors.accent.stops[ui::tokens::colors::ACCENT_PRIMARY_IDX]
                } else {
                    tokens.colors.surface_white_10
                };
                scene.fill(Fill::NonZero, transform, bg, None, &RoundedRect::from_rect(cell, ROW_RADIUS * self.scale));
            }
            // Icon centred in the upper portion.
            let icon_x = cell.x0 + (self.cell_w() - icon_px) / 2.0;
            let icon_y = cell.y0 + 14.0 * self.scale;
            let icon_color = if selected {
                Color::WHITE
            } else if entry.is_dir {
                tokens.colors.accent.stops[ui::tokens::colors::ACCENT_PRIMARY_IDX]
            } else {
                tokens.colors.text_primary
            };
            let file_icon_rect = Rect::new(icon_x, icon_y, icon_x + icon_px, icon_y + icon_px);
            draw_icon(
                scene,
                transform,
                file_icon_rect,
                icon_for_path(&entry.path, entry.is_dir),
                icon_color,
                LUCIDE_DEFAULT_STROKE_PX * self.scale.max(1.0),
            );
            // Sync-state emblem for workspace files (cloud / downloading / local).
            if let Some(state) = entry.sync_state {
                draw_sync_badge(scene, transform, file_icon_rect, state, self.scale, theme);
            }
            // Label centred under the icon, truncated to the cell width.
            let ly = icon_y + icon_px + 10.0 * self.scale;
            if self.renaming == Some(i) {
                // Publish the label field's rect and omit the label; the host
                // draws the in-place rename editor here.
                if let Some(sink) = &self.rename_rect {
                    sink.set(Some(Rect::new(
                        cell.x0 + 6.0,
                        ly - 4.0,
                        cell.x1 - 6.0,
                        ly + size as f64 + 6.0,
                    )));
                }
            } else {
                let label = truncate(tr, &entry.name, size, self.cell_w() - 10.0);
                let (lw, _) = tr.measure(&label, size, None);
                let lx = cell.x0 + (self.cell_w() - lw as f64) / 2.0;
                tr.draw_text(
                    scene,
                    &label,
                    size,
                    Self::cell_text(theme, selected),
                    transform * Affine::translate((lx, ly)),
                    None,
                );
            }
        }
    }
}

impl Widget for FileView {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Fill the parent width (so the grid's column count is derived from the
        // real pane width, never collapsing to one column) and measure the
        // content height.
        //
        // `align_self: Start` is load-bearing for scrolling: the enclosing
        // `Scrollable` is a default flex container (row direction,
        // `align-items: stretch`), which would otherwise stretch this leaf's
        // cross axis (its height) to the viewport. Stretched to the viewport,
        // the view reports a height equal to the visible area, so the
        // Scrollable sees `content == viewport` and a long directory can't
        // scroll at all. Opting out of the stretch lets the leaf take its
        // *measured* content height (all the rows / every grid row), so the
        // Scrollable's content overflows the viewport and the wheel +
        // scrollbar thumb move the list.
        let node = taffy
            .new_leaf(Style {
                size: Size { width: Dimension::percent(1.0), height: Dimension::auto() },
                align_self: Some(taffy::AlignSelf::Start),
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        _tr: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let width = known.width.map(|w| w as f64).unwrap_or(match available.width {
            taffy::AvailableSpace::Definite(w) => w as f64,
            _ => self.cell_w() + 2.0 * self.pad_x(),
        });
        if self.mode == ViewMode::Grid {
            self.columns = self.columns_for_width(width);
        }
        taffy::Size { width: width as f32, height: self.content_height() as f32 }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
        if self.mode == ViewMode::Grid {
            self.columns = self.columns_for_width(self.rect.width());
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // The enclosing `Scrollable` publishes its viewport rect here; cull
        // rows/cells outside it so a long directory only paints what's on
        // screen (the wheel scrolls thousands of files without paying to emit
        // every off-screen row's icon + text runs each frame). Remember it so
        // the header hit-test can follow the pinned header on the next event.
        let vp = presence.viewport.get();
        self.last_vp = vp;
        // Clear last frame's rename-field rect; the draw below republishes it
        // only if the renaming row is actually on screen.
        if let Some(sink) = &self.rename_rect {
            sink.set(None);
        }
        match self.mode {
            ViewMode::List => self.draw_list(scene, transform, tr, theme, vp),
            ViewMode::Grid => self.draw_grid(scene, transform, tr, theme, vp),
        }
        if self.is_focused {
            if let Some(i) = self.selection.primary() {
                let r = match self.mode {
                    ViewMode::List => {
                        let row = self.list_row_rect(i);
                        Rect::new(row.x0 + 6.0, row.y0 + 2.0, row.x1 - 6.0, row.y1 - 2.0)
                    }
                    ViewMode::Grid => self.cell_rect(i),
                };
                if !cull(vp, r) {
                    scene.stroke(
                        &Stroke::new(2.0),
                        transform,
                        theme.border_focus,
                        None,
                        &RoundedRect::from_rect(r, ROW_RADIUS),
                    );
                }
            }
        }
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
                self.last_cursor = pt;
                let new_hover = self.item_at(pt);
                if new_hover != self.hover {
                    self.hover = new_hover;
                }
                // A held directory press that travels far enough starts a
                // drag-to-pin; the app carries the folder toward the sidebar.
                if let Some((start, path)) = self.pressed_dir.clone() {
                    if pt.distance(start) > DRAG_THRESHOLD {
                        push(&self.sink, FileCommand::BeginDrag(path));
                        self.pressed_dir = None;
                    }
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                // Header click → sort.
                if let Some(col) = self.header_sort_at(self.last_cursor) {
                    push(&self.sink, FileCommand::SortColumn(col));
                    *handled = true;
                    return;
                }
                if let Some(i) = self.item_at(self.last_cursor) {
                    let ctrl = modifiers.state().control_key() || modifiers.state().super_key();
                    let shift = modifiers.state().shift_key();
                    // Reflect the click locally so this frame paints selected,
                    // and let the app own the authoritative selection + the
                    // single/double-click distinction.
                    self.selection.apply_click(i, ctrl, shift);
                    *focus_target = Some(node_id);
                    push(&self.sink, FileCommand::Click { index: i, ctrl, shift });
                    // A plain press on a folder arms a potential drag-to-pin.
                    if !ctrl && !shift {
                        if let Some(entry) = self.entries.get(i) {
                            if entry.is_dir {
                                self.pressed_dir = Some((self.last_cursor, entry.path.clone()));
                            }
                        }
                    }
                    *handled = true;
                }
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                // A release without travel was just a click — disarm the drag.
                self.pressed_dir = None;
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Right, .. } => {
                // Select the row under the cursor before the context menu opens
                // (the app opens the menu after draining this) so the menu's
                // actions target what was right-clicked, not a stale selection.
                let target = self.item_at(self.last_cursor);
                if let Some(i) = target {
                    if !self.selection.is_selected(i) {
                        self.selection.select_only(i);
                    }
                    *focus_target = Some(node_id);
                }
                push(&self.sink, FileCommand::ContextSelect(target));
                // Not marked handled: the app still needs to see the right-press
                // to open the menu.
            }
            _ => {}
        }
    }

    fn name(&self) -> &str { "FileView" }
    fn test_id(&self) -> Option<&str> { Some("file_view") }
    fn get_rect(&self) -> Rect { self.rect }
    fn is_focusable(&self) -> bool { true }
    fn set_focus(&mut self, f: bool) { self.is_focused = f; }
    fn is_focused(&self) -> bool { self.is_focused }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let primary = self.selection.primary();
        let value = primary.and_then(|i| self.entries.get(i)).map(|e| e.name.clone());
        let mut node = AccessNode::new(Role::List)
            .with_optional_value(value)
            .with_state(AccessState {
                focusable: true,
                focused: self.is_focused,
                ..Default::default()
            });
        if let Some(i) = primary {
            if i < self.entries.len() {
                node = node.with_position(i + 1, self.entries.len());
            }
        }
        Some(node)
    }
}

/// The badge glyph + colour for a workspace file's sync state. Follows the
/// familiar cloud-storage emblem language: a filled arrow/tick for on-disk, a
/// cloud for remote-only, a download arrow while hydrating.
fn sync_badge(state: SyncState, theme: &Theme) -> (Icon, Color) {
    let accent = theme.tokens.colors.accent.stops[ui::tokens::colors::ACCENT_PRIMARY_IDX];
    match state {
        // On disk: a quiet tick — nothing the user needs to act on.
        SyncState::Local => (Icon::Check, theme.tokens.colors.text_dim),
        // Remote-only: a cloud, tinted so it reads as "not here yet".
        SyncState::Projected => (Icon::CloudDownload, theme.tokens.colors.text_dim),
        // Actively hydrating: a download arrow in the accent to draw the eye.
        SyncState::Downloading => (Icon::Download, accent),
    }
}

/// Paint the sync-state emblem over the bottom-right corner of a file's icon,
/// on an opaque disc so it stays legible over a selected row's accent fill.
/// `icon_rect` is the file icon's box; `scale` sizes the badge with the icon.
fn draw_sync_badge(
    scene: &mut Scene,
    transform: Affine,
    icon_rect: Rect,
    state: SyncState,
    scale: f64,
    theme: &Theme,
) {
    let (icon, color) = sync_badge(state, theme);
    // The badge overlaps the icon's lower-right corner (the standard emblem
    // position); size it relative to the icon so it scales with zoom.
    let bsz = (icon_rect.width() * 0.62).max(9.0);
    let cx = icon_rect.x1;
    let cy = icon_rect.y1;
    // Opaque backing disc so the emblem reads over any row highlight.
    scene.fill(
        Fill::NonZero,
        transform,
        theme.tokens.colors.surfaces.base,
        None,
        &Circle::new((cx, cy), bsz * 0.62),
    );
    let br = Rect::new(cx - bsz / 2.0, cy - bsz / 2.0, cx + bsz / 2.0, cy + bsz / 2.0);
    draw_icon(scene, transform, br, icon, color, LUCIDE_DEFAULT_STROKE_PX * scale.max(1.0));
}

/// Cull a row/cell whose vertical extent falls entirely outside the scroll
/// viewport `vp` (the enclosing `Scrollable`'s clip rect, published via the
/// presence context). A small margin keeps the row just off each edge alive so
/// a partially-visible row at the boundary always paints. When no viewport is
/// active (a plain, un-scrolled draw) nothing is culled.
fn cull(vp: Option<Rect>, r: Rect) -> bool {
    match vp {
        Some(v) => {
            const MARGIN: f64 = 4.0;
            r.y1 < v.y0 - MARGIN || r.y0 > v.y1 + MARGIN
        }
        None => false,
    }
}

/// Trim `text` with an ellipsis until it fits `max_w`.
fn truncate(tr: &mut TextRenderer, text: &str, size: f32, max_w: f64) -> String {
    if max_w <= 0.0 {
        return String::new();
    }
    let (w, _) = tr.measure(text, size, None);
    if (w as f64) <= max_w {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut keep = chars.len();
    while keep > 0 {
        keep -= 1;
        let candidate: String = chars[..keep].iter().collect::<String>() + "\u{2026}";
        let (cw, _) = tr.measure(&candidate, size, None);
        if (cw as f64) <= max_w {
            return candidate;
        }
    }
    "\u{2026}".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{new_sink, FileCommand};
    use std::path::PathBuf;

    fn dev() -> winit::event::DeviceId {
        unsafe { std::mem::zeroed() }
    }

    fn dispatch(v: &mut FileView, tree: &TaffyTree<()>, node: NodeId, ev: WindowEvent) {
        let mut handled = false;
        let mut focus = None;
        let mut doc = automerge::AutoCommit::new();
        let root = automerge::ROOT;
        let mods = winit::event::Modifiers::default();
        v.handle_event(&ev, 1.0, &mods, &mut doc, &root, &mut handled, &mut focus, node, tree);
    }

    fn ent(name: &str, is_dir: bool) -> Entry {
        Entry {
            name: name.into(),
            path: PathBuf::from("/x").join(name),
            is_dir,
            size: 10,
            modified: None,
            mime: String::new(),
            sync_state: None,
        }
    }

    fn items(n: usize) -> Vec<Entry> {
        (0..n).map(|i| ent(&format!("f{i}"), false)).collect()
    }

    fn grid(n: usize, width: f64) -> FileView {
        let sink = new_sink();
        let sel = SelectionModel::new();
        let mut v = FileView::new(&items(n), &sel, &sink).mode(ViewMode::Grid);
        v.rect = Rect::new(0.0, 0.0, width, 600.0);
        v.columns = v.columns_for_width(width);
        v
    }

    #[test]
    fn grid_flows_left_to_right_and_wraps_to_fill_width() {
        // A wide pane must lay several columns per row, not one item per line.
        let v = grid(12, 900.0);
        assert!(v.columns >= 5, "wide grid uses many columns, got {}", v.columns);
        // Item 0 and item 1 sit on the SAME row (item 1 is to the right of 0),
        // proving row-major flow rather than a vertical stack.
        let c0 = v.cell_rect(0);
        let c1 = v.cell_rect(1);
        assert!((c0.y0 - c1.y0).abs() < 1e-6, "items 0 and 1 share a row");
        assert!(c1.x0 > c0.x0, "item 1 is to the right of item 0");
        // The item after a full row wraps down to the next row at the left.
        let wrap = v.cell_rect(v.columns);
        assert!(wrap.y0 > c0.y0, "item after a full row wraps to the next row");
        assert!((wrap.x0 - c0.x0).abs() < 1e-6, "wrapped item returns to the left edge");
    }

    #[test]
    fn grid_columns_scale_with_width_and_never_zero() {
        assert!(grid(20, 1200.0).columns > grid(20, 400.0).columns);
        assert!(grid(20, 10.0).columns >= 1, "never collapses below one column");
    }

    #[test]
    fn zoom_scale_grows_row_height_and_cell_size() {
        let sink = new_sink();
        let sel = SelectionModel::new();
        let base = FileView::new(&items(3), &sel, &sink);
        let zoomed = FileView::new(&items(3), &sel, &sink).with_scale(1.6);
        assert!(zoomed.row_h() > base.row_h(), "zoom grows list row height");
        assert!(zoomed.cell_w() > base.cell_w(), "zoom grows grid cell width");
        assert!(zoomed.cell_h() > base.cell_h());
    }

    #[test]
    fn narrow_list_collapses_detail_columns_to_give_name_room() {
        let sink = new_sink();
        let sel = SelectionModel::new();
        let mut v = FileView::new(&items(4), &sel, &sink);
        // Wide: all three columns shown.
        v.rect = Rect::new(0.0, 0.0, 900.0, 400.0);
        assert!(v.show_size() && v.show_modified(), "wide pane shows every column");

        // Medium: Modified collapses; the right-anchored Size takes its place and
        // the Name column reclaims the freed space up to the Size column.
        v.rect = Rect::new(0.0, 0.0, 460.0, 400.0);
        assert!(v.show_size(), "size still shown at medium width");
        assert!(!v.show_modified(), "modified collapses on a narrow pane");
        let size = v.header_col_rect(1);
        assert!(
            size.x1 >= v.rect.x1 - v.pad_x() - 1.0,
            "size is right-anchored once modified is hidden",
        );
        let name = v.header_col_rect(0);
        assert!(name.x1 <= size.x0 && name.x1 > size.x0 - v.pad_x() - 12.0, "name reclaims up to the size column");

        // Very narrow: both detail columns collapse; Name spans the whole pane.
        v.rect = Rect::new(0.0, 0.0, 300.0, 400.0);
        assert!(!v.show_size() && !v.show_modified(), "very narrow shows name only");
        let name = v.header_col_rect(0);
        assert!(name.x1 >= v.rect.x1 - v.pad_x() - 1.0, "name fills the pane when both details hide");
    }

    #[test]
    fn rows_outside_the_scroll_viewport_are_culled() {
        // The paint path calls `cull(vp, row)` for every row/cell; a long
        // directory must only pay to draw what the viewport actually shows.
        let vp = Some(Rect::new(0.0, 100.0, 600.0, 400.0));
        // Well above the viewport → culled.
        assert!(cull(vp, Rect::new(0.0, 0.0, 600.0, 20.0)));
        // Well below → culled.
        assert!(cull(vp, Rect::new(0.0, 500.0, 600.0, 520.0)));
        // Squarely inside → drawn.
        assert!(!cull(vp, Rect::new(0.0, 200.0, 600.0, 220.0)));
        // Straddling the top edge (within the keep-alive margin) → drawn.
        assert!(!cull(vp, Rect::new(0.0, 98.0, 600.0, 102.0)));
        // No active viewport (a plain, un-scrolled draw) → never culled.
        assert!(!cull(None, Rect::new(0.0, 99_999.0, 600.0, 100_000.0)));
    }

    #[test]
    fn header_pins_to_the_viewport_top_once_scrolled() {
        let sink = new_sink();
        let sel = SelectionModel::new();
        let mut v = FileView::new(&items(40), &sel, &sink);
        let vp = Some(Rect::new(0.0, 0.0, 600.0, 300.0));
        // At rest the content's top aligns with the viewport top → the header
        // sits at the content top.
        v.rect = Rect::new(0.0, 0.0, 600.0, 800.0);
        assert_eq!(v.header_top(vp), 0.0);
        // Scrolled: the content is pushed up (rect.y0 < viewport top) so the
        // header pins to the viewport's top edge and stays visible.
        v.rect = Rect::new(0.0, -250.0, 600.0, 550.0);
        assert_eq!(v.header_top(vp), 0.0, "pinned to the viewport top");
        // Without a viewport the header just tracks the content top.
        assert_eq!(v.header_top(None), -250.0);
    }

    #[test]
    fn header_click_sorts_even_after_the_list_is_scrolled() {
        let sink = new_sink();
        let sel = SelectionModel::new();
        let mut v = FileView::new(&items(40), &sel, &sink);
        // Scrolled up by 250px; the viewport starts at y=0.
        v.rect = Rect::new(0.0, -250.0, 900.0, 550.0);
        v.last_vp = Some(Rect::new(0.0, 0.0, 900.0, 300.0));
        // A click on the pinned Name header (in the viewport-top band) still
        // resolves to the name sort column.
        let name = v.header_col_rect(0);
        let pt = Point::new(name.x0 + 5.0, v.header_h() / 2.0);
        assert_eq!(v.header_sort_at(pt), Some(1), "pinned Name header sorts");
        // A click where the header *used* to be (scrolled off the top) does not
        // sort — it's off-screen above the viewport.
        assert_eq!(
            v.header_sort_at(Point::new(name.x0 + 5.0, -250.0 + v.header_h() / 2.0)),
            None,
        );
    }

    #[test]
    fn right_press_selects_the_row_under_the_cursor_for_the_menu() {
        // The dangerous bug this guards: a right-click must target the row it
        // lands on, not whatever was selected before. The FileView emits a
        // `ContextSelect(Some(row))` the app drains before opening the menu.
        let sink = new_sink();
        let sel = SelectionModel::new();
        let mut v = FileView::new(&items(10), &sel, &sink);
        v.rect = Rect::new(0.0, 0.0, 600.0, 600.0);

        let mut tree = TaffyTree::<()>::new();
        let node = tree.new_leaf(Style::default()).unwrap();
        // Move the cursor onto row 3, then right-press.
        let y = v.rect.y0 + v.header_h() + 3.0 * v.row_h() + v.row_h() / 2.0;
        dispatch(&mut v, &tree, node, WindowEvent::CursorMoved { device_id: dev(), position: (50.0, y).into() });
        dispatch(&mut v, &tree, node, WindowEvent::MouseInput { device_id: dev(), state: ElementState::Pressed, button: MouseButton::Right });

        let cmds = sink.borrow();
        assert!(
            cmds.iter().any(|c| matches!(c, FileCommand::ContextSelect(Some(3)))),
            "right-press emits ContextSelect for the row it lands on: {cmds:?}",
        );
    }

    #[test]
    fn right_press_on_empty_space_clears_the_selection_for_the_menu() {
        let sink = new_sink();
        let sel = SelectionModel::new();
        let mut v = FileView::new(&items(3), &sel, &sink);
        v.rect = Rect::new(0.0, 0.0, 600.0, 600.0);

        let mut tree = TaffyTree::<()>::new();
        let node = tree.new_leaf(Style::default()).unwrap();
        // Right-press well below the 3 rows → empty space.
        let y = v.rect.y0 + v.header_h() + 40.0 * v.row_h();
        dispatch(&mut v, &tree, node, WindowEvent::CursorMoved { device_id: dev(), position: (50.0, y).into() });
        dispatch(&mut v, &tree, node, WindowEvent::MouseInput { device_id: dev(), state: ElementState::Pressed, button: MouseButton::Right });

        let cmds = sink.borrow();
        assert!(
            cmds.iter().any(|c| matches!(c, FileCommand::ContextSelect(None))),
            "right-press on empty space clears the selection: {cmds:?}",
        );
    }

    #[test]
    fn sync_badge_maps_each_state_to_a_distinct_emblem() {
        let theme = Theme::dark();
        // Each hydration state gets its own recognisable glyph, and the
        // downloading state is tinted with the accent to catch the eye.
        assert_eq!(sync_badge(SyncState::Local, &theme).0, Icon::Check);
        assert_eq!(sync_badge(SyncState::Projected, &theme).0, Icon::CloudDownload);
        let (dl_icon, dl_color) = sync_badge(SyncState::Downloading, &theme);
        assert_eq!(dl_icon, Icon::Download);
        assert_eq!(
            dl_color,
            theme.tokens.colors.accent.stops[ui::tokens::colors::ACCENT_PRIMARY_IDX],
            "downloading badge uses the accent colour",
        );
    }

    #[test]
    fn dragging_a_directory_row_begins_a_pin_drag() {
        let sink = new_sink();
        let sel = SelectionModel::new();
        let mut entries = items(5);
        entries[2] = ent("folder", true);
        let mut v = FileView::new(&entries, &sel, &sink);
        v.rect = Rect::new(0.0, 0.0, 600.0, 600.0);
        let mut tree = TaffyTree::<()>::new();
        let node = tree.new_leaf(Style::default()).unwrap();

        // Press on the folder row (row 2), then travel past the threshold.
        let y = v.rect.y0 + v.header_h() + 2.0 * v.row_h() + v.row_h() / 2.0;
        dispatch(&mut v, &tree, node, WindowEvent::CursorMoved { device_id: dev(), position: (50.0, y).into() });
        dispatch(&mut v, &tree, node, WindowEvent::MouseInput { device_id: dev(), state: ElementState::Pressed, button: MouseButton::Left });
        dispatch(&mut v, &tree, node, WindowEvent::CursorMoved { device_id: dev(), position: (120.0, y).into() });

        let cmds = sink.borrow();
        assert!(
            cmds.iter().any(|c| matches!(c, FileCommand::BeginDrag(p) if p.ends_with("folder"))),
            "a folder press that travels begins a pin drag: {cmds:?}",
        );
    }

    #[test]
    fn a_file_row_press_does_not_begin_a_pin_drag() {
        // Only folders can be pinned to Favourites, so a file must not drag.
        let sink = new_sink();
        let sel = SelectionModel::new();
        let mut v = FileView::new(&items(5), &sel, &sink);
        v.rect = Rect::new(0.0, 0.0, 600.0, 600.0);
        let mut tree = TaffyTree::<()>::new();
        let node = tree.new_leaf(Style::default()).unwrap();
        let y = v.rect.y0 + v.header_h() + v.row_h() / 2.0;
        dispatch(&mut v, &tree, node, WindowEvent::CursorMoved { device_id: dev(), position: (50.0, y).into() });
        dispatch(&mut v, &tree, node, WindowEvent::MouseInput { device_id: dev(), state: ElementState::Pressed, button: MouseButton::Left });
        dispatch(&mut v, &tree, node, WindowEvent::CursorMoved { device_id: dev(), position: (120.0, y).into() });
        let cmds = sink.borrow();
        assert!(
            !cmds.iter().any(|c| matches!(c, FileCommand::BeginDrag(_))),
            "a file does not start a pin drag: {cmds:?}",
        );
    }

    #[test]
    fn list_item_at_maps_rows_below_the_header() {
        let sink = new_sink();
        let sel = SelectionModel::new();
        let mut v = FileView::new(&items(4), &sel, &sink);
        v.rect = Rect::new(0.0, 0.0, 600.0, 400.0);
        // A point inside the header band selects nothing.
        assert_eq!(v.item_at(Point::new(50.0, v.header_h() - 2.0)), None);
        // The first row sits just below the header.
        let y = v.rect.y0 + v.header_h() + v.row_h() / 2.0;
        assert_eq!(v.item_at(Point::new(50.0, y)), Some(0));
        // The third row.
        let y2 = v.rect.y0 + v.header_h() + 2.0 * v.row_h() + v.row_h() / 2.0;
        assert_eq!(v.item_at(Point::new(50.0, y2)), Some(2));
    }
}
