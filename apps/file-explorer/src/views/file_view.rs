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

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Blob, Color, Fill, ImageAlphaType, ImageData as PenikoImage, ImageFormat};
use winit::event::{ElementState, MouseButton, WindowEvent};

use thumbnail::{FileCategory, ThumbKey, ThumbState, ThumbnailService, category_for_path};

use crate::command::{CommandSink, FileCommand, push};
use crate::i18n::{arg, t, t_args};
use crate::model::{
    AvailabilityPolicy, AvailabilityProjection, Entry, ReplicaEvidence, Residency, SortBy,
    SortOrder, SyncState, TransferPhase, ViewMode, format_modified, human_size,
};
use ui::SelectionModel;
use ui::icons::{Icon, LUCIDE_DEFAULT_STROKE_PX, draw_icon, icon_for_path};
use ui::image_store::paint_image_cover;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

/// The file manager's handle onto the shared thumbnail system: the off-thread
/// service (cache + workers) plus an app-lived memo that converts a decoded
/// [`thumbnail::RgbaThumb`] into a `peniko` image exactly once (so a painted
/// grid never rebuilds the GPU-facing blob per frame). Cheap to clone; the view
/// is handed a fresh copy each rebuild.
#[derive(Clone)]
pub struct ThumbCtx {
    pub service: Arc<ThumbnailService>,
    pub imgs: Rc<RefCell<HashMap<ThumbKey, PenikoImage>>>,
}

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

/// A folder drop target the view publishes each layout: a directory row's
/// absolute on-screen rect and path. The host owns the drag (it captures the
/// pointer once a drag begins, so the view never sees the release), so it needs
/// the view to tell it where each folder sits to resolve what a drop lands on.
#[derive(Clone, Debug)]
pub struct FolderDropTarget {
    pub path: PathBuf,
    pub rect: Rect,
}

/// Shared, host-owned sink the view republishes its folder rows into each
/// layout (cleared + refilled), analogous to the rename-rect channel.
pub type DropTargets = Rc<RefCell<Vec<FolderDropTarget>>>;

/// Per-item inputs to [`FileView::draw_file_visual`], bundled into one
/// argument so the paint call stays under clippy's argument-count limit.
#[derive(Clone, Copy)]
struct FileVisual<'a> {
    rect: Rect,
    entry: &'a Entry,
    selected: bool,
    glyph_color: Color,
    grid: bool,
}

/// Layout already resolved by [`FileView::draw_list_row`], handed to
/// [`FileView::draw_list_row_text`] so that helper stays under clippy's
/// argument-count limit.
#[derive(Clone, Copy)]
struct RowTextLayout {
    i: usize,
    row: Rect,
    name_rect: Rect,
    name_x: f64,
    selected: bool,
}

/// A list column the header exposes for click-to-sort. `sort_col` is the index
/// the app's `SortColumn` command understands (1 = name, 2 = size, 3 = date).
struct ColSpec {
    header_key: &'static str,
    sort_col: usize,
    sort_by: SortBy,
}

const LIST_COLUMNS: [ColSpec; 3] = [
    ColSpec {
        header_key: "files-column-name",
        sort_col: 1,
        sort_by: SortBy::Name,
    },
    ColSpec {
        header_key: "files-column-size",
        sort_col: 2,
        sort_by: SortBy::Size,
    },
    ColSpec {
        header_key: "files-column-modified",
        sort_col: 3,
        sort_by: SortBy::Modified,
    },
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
    /// A plain press on a row `(press point, its path, whether it is a folder)`,
    /// held until the pointer either releases (a click) or travels past the drag
    /// threshold (begins a drag — to move the row onto a folder, or to pin a
    /// folder to the sidebar).
    pressed_row: Option<(Point, PathBuf, bool)>,
    /// Host-owned sink for this view's folder rows, republished each layout so
    /// the host can resolve which folder a drag is dropped onto.
    drop_targets: Option<DropTargets>,
    /// The shared thumbnail service + peniko memo. `None` disables previews (the
    /// view falls back to type glyphs/badges), which keeps the widget usable in
    /// contexts without a thumbnail service.
    thumbs: Option<ThumbCtx>,
}

/// Pointer travel (logical px) past which a held directory press becomes a
/// drag rather than a click.
pub const DRAG_THRESHOLD: f64 = 6.0;

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
            pressed_row: None,
            drop_targets: None,
            thumbs: None,
        }
    }

    /// Bind the host's folder-drop-target sink; the view republishes its folder
    /// rows into it each layout so the host can hit-test a drag's drop point.
    #[must_use]
    pub fn with_drop_targets(mut self, sink: DropTargets) -> Self {
        self.drop_targets = Some(sink);
        self
    }

    /// Bind the shared thumbnail service (+ the app's peniko memo) so file cells
    /// paint real previews for images/PDF/ebook (and host-provided previews for
    /// workspace files), falling back to a type badge otherwise.
    #[must_use]
    pub fn with_thumbs(mut self, thumbs: Option<ThumbCtx>) -> Self {
        self.thumbs = thumbs;
        self
    }

    #[must_use]
    pub fn mode(mut self, mode: ViewMode) -> Self {
        self.mode = mode;
        self
    }

    /// Render item `index` (if any) as an in-place rename: omit its label and
    /// publish the label's on-screen rect into `sink` so the host can draw the
    /// shared rename editor over it.
    #[must_use]
    pub fn with_rename(mut self, index: Option<usize>, sink: Rc<Cell<Option<Rect>>>) -> Self {
        self.renaming = index;
        self.rename_rect = Some(sink);
        self
    }
    #[must_use]
    pub fn with_scale(mut self, scale: f64) -> Self {
        self.scale = scale.clamp(0.5, 3.0);
        self
    }

    /// Seed the pointer position the host last saw. The host rebuilds this view
    /// from scratch every frame, so without this a fresh view starts with the
    /// cursor at the origin — and a press that arrives with no `CursorMoved`
    /// since the rebuild (a *still* double-click's second press) would hit-test
    /// against `(0, 0)` and miss the row, so the folder never opens. Seeding the
    /// real position makes press hit-testing correct from the first event.
    #[must_use]
    pub fn with_cursor(mut self, cursor: Point) -> Self {
        self.last_cursor = cursor;
        self
    }
    #[must_use]
    pub fn with_sort(mut self, by: SortBy, order: SortOrder) -> Self {
        self.sort_by = by;
        self.sort_order = order;
        self
    }

    // --- scaled metrics ---------------------------------------------------

    fn row_h(&self) -> f64 {
        LIST_ROW_H * self.scale
    }
    fn header_h(&self) -> f64 {
        LIST_HEADER_H * self.scale
    }
    fn cell_w(&self) -> f64 {
        GRID_CELL_W * self.scale
    }
    fn cell_h(&self) -> f64 {
        GRID_CELL_H * self.scale
    }
    fn grid_gap(&self) -> f64 {
        GRID_GAP * self.scale
    }
    fn pad_x(&self) -> f64 {
        CONTENT_PAD_X * self.scale
    }
    fn body_text(&self, theme: &Theme) -> f32 {
        (f64::from(theme.tokens.typography.scale.base) * self.scale) as f32
    }
    fn header_text(theme: &Theme) -> f32 {
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
        if self.entries.is_empty() {
            0
        } else {
            self.entries.len().div_ceil(columns.max(1))
        }
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

    /// Republish the absolute rect of every folder row into the host's drop-
    /// target sink, so the host can resolve which folder a drag is released
    /// over. Only directories are targets — you can't drop a file *into* a file.
    fn publish_drop_targets(&self) {
        let Some(sink) = &self.drop_targets else {
            return;
        };
        let mut out = Vec::new();
        for (i, entry) in self.entries.iter().enumerate() {
            if !entry.is_dir {
                continue;
            }
            let rect = match self.mode {
                ViewMode::List => self.list_row_rect(i),
                ViewMode::Grid => self.cell_rect(i),
            };
            out.push(FolderDropTarget {
                path: entry.path.clone(),
                rect,
            });
        }
        *sink.borrow_mut() = out;
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
                if rel_x - col as f64 * sx > self.cell_w()
                    || rel_y - row as f64 * sy > self.cell_h()
                {
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
        if selected {
            Color::WHITE
        } else {
            theme.tokens.colors.text_primary
        }
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
    fn draw_list_header(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        vp: Option<Rect>,
    ) {
        let tokens = &theme.tokens;
        let header_h = self.header_h();
        let hsize = Self::header_text(theme);
        let top = self.header_top(vp);
        // Opaque band matching the content background so scrolled rows never
        // bleed through the (transparent-text) header.
        let band = Rect::new(self.rect.x0, top, self.rect.x1, top + header_h);
        scene.fill(
            Fill::NonZero,
            transform,
            tokens.colors.surfaces.base,
            None,
            &band,
        );
        // Column labels + a sort caret on the active column. Hidden (collapsed)
        // columns are skipped.
        for (i, col) in LIST_COLUMNS.iter().enumerate() {
            if !self.col_visible(i) {
                continue;
            }
            let cr = self.header_col_rect(i);
            let ty = top + (header_h - f64::from(hsize)) / 2.0;
            let header = t(col.header_key);
            tr.draw_text(
                scene,
                &header,
                hsize,
                tokens.colors.text_dim,
                transform * Affine::translate((cr.x0, ty)),
                None,
            );
            if self.sort_by == col.sort_by {
                let caret = if self.sort_order == SortOrder::Ascending {
                    "\u{25B4}"
                } else {
                    "\u{25BE}"
                };
                let (lw, _) = tr.measure(&header, hsize, None);
                tr.draw_text(
                    scene,
                    caret,
                    hsize,
                    tokens.colors.accent.stops[ui::tokens::colors::ACCENT_PRIMARY_IDX],
                    transform * Affine::translate((cr.x0 + f64::from(lw) + 4.0, ty)),
                    None,
                );
            }
        }
        // Quiet header separator along the band's bottom edge.
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            ui::tokens::colors::Colors::hairline_on(tokens.colors.surfaces.raised, 26),
            None,
            &Line::new(
                (self.rect.x0 + self.pad_x(), top + header_h),
                (self.rect.x1 - self.pad_x(), top + header_h),
            ),
        );
    }

    fn draw_list(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        vp: Option<Rect>,
    ) {
        for (i, entry) in self.entries.iter().enumerate() {
            let row = self.list_row_rect(i);
            if cull(vp, row) {
                continue;
            }
            self.draw_list_row(scene, transform, tr, theme, i, entry);
        }
        // Header last so it composites above the rows (and stays put while
        // they scroll beneath the pinned band).
        self.draw_list_header(scene, transform, tr, theme, vp);
    }

    /// Paint list row `i` (icon, name, size, modified) — split out of
    /// [`Self::draw_list`] purely to stay under the line-count limit.
    fn draw_list_row(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        i: usize,
        entry: &Entry,
    ) {
        let tokens = &theme.tokens;
        let icon_size_px = LIST_ICON * self.scale;
        let row = self.list_row_rect(i);
        let selected = self.selection.is_selected(i);
        let hovered = self.hover == Some(i) && !selected;
        // Row highlight — inset so it reads as a pill with breathing room.
        if selected || hovered {
            let hl = Rect::new(row.x0 + 6.0, row.y0 + 2.0, row.x1 - 6.0, row.y1 - 2.0);
            let bg = if selected {
                tokens.colors.accent.stops[ui::tokens::colors::ACCENT_PRIMARY_IDX]
            } else {
                tokens.colors.lift(0.72)
            };
            scene.fill(
                Fill::NonZero,
                transform,
                bg,
                None,
                &RoundedRect::from_rect(hl, ROW_RADIUS),
            );
        }

        // Icon + name.
        let name_rect = self.header_col_rect(0);
        let icon_x = name_rect.x0 + 4.0;
        let icon_y = row.y0 + (row.height() - icon_size_px) / 2.0;
        let icon_color = if selected {
            Color::WHITE
        } else if entry.is_dir {
            tokens.colors.accent.stops[ui::tokens::colors::ACCENT_PRIMARY_IDX]
        } else {
            tokens.colors.text_dim
        };
        let file_icon_rect =
            Rect::new(icon_x, icon_y, icon_x + icon_size_px, icon_y + icon_size_px);
        // A real thumbnail when ready, else the compact type glyph.
        self.draw_file_visual(
            scene,
            transform,
            tr,
            theme,
            FileVisual {
                rect: file_icon_rect,
                entry,
                selected,
                glyph_color: icon_color,
                grid: false,
            },
        );
        // Sync-state emblem for workspace files (cloud / downloading / local).
        if let Some(availability) = &entry.availability {
            draw_availability_badge(
                scene,
                transform,
                file_icon_rect,
                availability,
                self.scale,
                theme,
            );
        } else if let Some(state) = entry.sync_state {
            draw_sync_badge(scene, transform, file_icon_rect, state, self.scale, theme);
        }
        let name_x = icon_x + icon_size_px + 10.0;
        self.draw_list_row_text(
            scene,
            transform,
            tr,
            theme,
            entry,
            RowTextLayout {
                i,
                row,
                name_rect,
                name_x,
                selected,
            },
        );
    }

    /// Paint row `layout.i`'s name (or the in-place rename rect), size and
    /// modified columns — the rest of [`Self::draw_list_row`], split out
    /// purely to stay under the line-count limit.
    fn draw_list_row_text(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        entry: &Entry,
        layout: RowTextLayout,
    ) {
        let RowTextLayout {
            i,
            row,
            name_rect,
            name_x,
            selected,
        } = layout;
        let size = self.body_text(theme);
        // Optically center the row text so it lines up with the icon, which is
        // centered by its box (see `crate::text_layout`).
        let text_y = crate::text_layout::centered_text_y(row.y0, row.height(), size);
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

    // --- thumbnails -------------------------------------------------------

    /// The content-addressed cache key for `entry` at the service's target size.
    /// Local files key on `(path, mtime, size)` — hashing the whole file on the
    /// UI thread would be the jank the off-thread service exists to avoid, and
    /// that tuple still changes whenever the content does.
    fn thumb_key(ctx: &ThumbCtx, entry: &Entry) -> ThumbKey {
        if let Some(hash) = entry.content_hash {
            return ThumbKey::of_hash(hash, ctx.service.edge());
        }
        let mtime_ms = entry
            .modified
            .map_or(0, |t| t.timestamp_millis().max(0) as u64);
        ThumbKey::of_path_meta(&entry.path, mtime_ms, entry.size, ctx.service.edge())
    }

    /// Request `entry`'s preview from the service. Workspace-mount files go
    /// through the **host-provided** metadata channel (the `user.qos.thumb`
    /// xattr) — the file manager never re-decodes what the host already
    /// previewed, and a projected file is never hydrated just to preview it.
    /// Ordinary local files are generated by the shared service directly.
    fn request_thumb(
        ctx: &ThumbCtx,
        entry: &Entry,
        key: ThumbKey,
        category: FileCategory,
    ) -> ThumbState {
        Self::request_thumb_in(
            ctx,
            entry,
            key,
            category,
            crate::model::is_workspace_path(&entry.path),
        )
    }

    /// The request body, with the workspace decision passed in — split out on
    /// the same terms as [`crate::model::read_dir_sync`] so the routing is
    /// testable without mutating the process environment. Production always
    /// reaches it through [`Self::request_thumb`].
    fn request_thumb_in(
        ctx: &ThumbCtx,
        entry: &Entry,
        key: ThumbKey,
        category: FileCategory,
        in_workspace: bool,
    ) -> ThumbState {
        if !in_workspace {
            return ctx.service.request_file(key, category, entry.path.clone());
        }
        let path = entry.path.clone();
        if entry.content_is_local() {
            let fallback = path.clone();
            ctx.service
                .request_encoded_or_file(key, category, fallback, move || {
                    crate::model::read_thumb_xattr(&path)
                })
        } else {
            // Projected content may use a separately available host preview,
            // but the fallback deliberately has no source-file branch.
            ctx.service
                .request_encoded(key, category, move || crate::model::read_thumb_xattr(&path))
        }
    }

    /// The `peniko` image for a ready thumbnail, converted once and memoised by
    /// key so a scrolling grid never rebuilds the blob per frame.
    fn thumb_peniko(ctx: &ThumbCtx, key: ThumbKey, rgba: &thumbnail::RgbaThumb) -> PenikoImage {
        if let Some(img) = ctx.imgs.borrow().get(&key) {
            return img.clone();
        }
        let img = PenikoImage {
            data: Blob::new(Arc::new(rgba.rgba.clone())),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width: rgba.width,
            height: rgba.height,
        };
        ctx.imgs.borrow_mut().insert(key, img.clone());
        img
    }

    /// Paint a file cell's visual into `rect`: a real thumbnail when one is
    /// ready, a type badge otherwise. `glyph_color` is the fallback glyph tint
    /// (used for directories and the list mode's compact icon). `grid` enables
    /// the richer badge card (there is room for it only in grid mode).
    fn draw_file_visual(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        visual: FileVisual,
    ) {
        let FileVisual {
            rect,
            entry,
            selected,
            glyph_color,
            grid,
        } = visual;
        // Directories keep their accent folder glyph — no preview, no badge.
        if entry.is_dir {
            self.draw_glyph(scene, transform, rect, entry, glyph_color);
            return;
        }
        let Some(ctx) = self.thumbs.clone() else {
            if grid {
                self.draw_type_badge(scene, transform, tr, rect, entry, selected);
            } else {
                self.draw_glyph(scene, transform, rect, entry, glyph_color);
            }
            return;
        };
        let category = category_for_path(&entry.path);
        let key = Self::thumb_key(&ctx, entry);
        if let ThumbState::Ready(rgba) = Self::request_thumb(&ctx, entry, key, category) {
            let img = Self::thumb_peniko(&ctx, key, &rgba);
            let radius = if grid {
                8.0 * self.scale
            } else {
                4.0 * self.scale
            };
            paint_image_cover(scene, transform, rect, &img, radius);
            // A hairline frame gives a light-on-light photo a defined edge.
            let border = theme.tokens.colors.lift(0.72);
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                border,
                None,
                &RoundedRect::from_rect(rect, radius),
            );
            return;
        }
        // No real preview (yet): a badge in grid, a compact glyph in the list.
        if grid {
            self.draw_type_badge(scene, transform, tr, rect, entry, selected);
        } else {
            self.draw_glyph(scene, transform, rect, entry, glyph_color);
        }
    }

    /// The plain type glyph (the pre-thumbnail fallback).
    fn draw_glyph(
        &self,
        scene: &mut Scene,
        transform: Affine,
        rect: Rect,
        entry: &Entry,
        color: Color,
    ) {
        draw_icon(
            scene,
            transform,
            rect,
            icon_for_path(&entry.path, entry.is_dir),
            color,
            LUCIDE_DEFAULT_STROKE_PX * self.scale.max(1.0),
        );
    }

    /// A good-looking type badge for a file with no real preview: a soft
    /// category-tinted card, the type glyph, and the uppercased extension — so a
    /// `.zip` / `.rs` / `.mp4` tells its kind at a glance. Drawn live from the
    /// theme, so it reads in both light and dark.
    fn draw_type_badge(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        rect: Rect,
        entry: &Entry,
        selected: bool,
    ) {
        let category = category_for_path(&entry.path);
        let tint = category_tint(category);
        let radius = 8.0 * self.scale;
        let card = RoundedRect::from_rect(rect, radius);
        // Soft fill + hairline border. On a selected (accent-filled) cell, use a
        // translucent white so the badge stays legible on the accent.
        let (fill, fg) = if selected {
            (Color::from_rgba8(255, 255, 255, 46), Color::WHITE)
        } else {
            (tint.with_alpha(0.16), tint)
        };
        scene.fill(Fill::NonZero, transform, fill, None, &card);
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            fg.with_alpha(if selected { 0.7 } else { 0.5 }),
            None,
            &card,
        );
        // Glyph in the upper portion.
        let pad = rect.height() * 0.14;
        let glyph_h = rect.height() * 0.5;
        let gx = rect.x0 + (rect.width() - glyph_h) / 2.0;
        let glyph_rect = Rect::new(gx, rect.y0 + pad, gx + glyph_h, rect.y0 + pad + glyph_h);
        self.draw_glyph(scene, transform, glyph_rect, entry, fg);
        // Extension label centred below the glyph.
        let ext = ext_label(&entry.path);
        if !ext.is_empty() {
            let fs = (rect.height() * 0.2).clamp(8.0, 13.0) as f32;
            let (w, _) = tr.measure(&ext, fs, None);
            let tx = rect.x0 + (rect.width() - f64::from(w)) / 2.0;
            let ty = rect.y1 - pad - f64::from(fs);
            tr.draw_text(
                scene,
                &ext,
                fs,
                fg,
                transform * Affine::translate((tx, ty)),
                None,
            );
        }
    }

    fn draw_grid(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        vp: Option<Rect>,
    ) {
        let tokens = &theme.tokens;
        let size = self.body_text(theme);
        let icon_size_px = GRID_ICON * self.scale;
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
                    tokens.colors.lift(0.72)
                };
                scene.fill(
                    Fill::NonZero,
                    transform,
                    bg,
                    None,
                    &RoundedRect::from_rect(cell, ROW_RADIUS * self.scale),
                );
            }
            // A preview tile centred in the upper portion — larger than the bare
            // glyph so a real thumbnail reads well; directories keep the glyph.
            let preview_px = if entry.is_dir {
                icon_size_px
            } else {
                icon_size_px * 1.3
            };
            let icon_x = cell.x0 + (self.cell_w() - preview_px) / 2.0;
            let icon_y = cell.y0 + 12.0 * self.scale;
            let icon_color = if selected {
                Color::WHITE
            } else if entry.is_dir {
                tokens.colors.accent.stops[ui::tokens::colors::ACCENT_PRIMARY_IDX]
            } else {
                tokens.colors.text_primary
            };
            let file_icon_rect =
                Rect::new(icon_x, icon_y, icon_x + preview_px, icon_y + preview_px);
            self.draw_file_visual(
                scene,
                transform,
                tr,
                theme,
                FileVisual {
                    rect: file_icon_rect,
                    entry,
                    selected,
                    glyph_color: icon_color,
                    grid: true,
                },
            );
            // Sync-state emblem for workspace files (cloud / downloading / local).
            if let Some(availability) = &entry.availability {
                draw_availability_badge(
                    scene,
                    transform,
                    file_icon_rect,
                    availability,
                    self.scale,
                    theme,
                );
            } else if let Some(state) = entry.sync_state {
                draw_sync_badge(scene, transform, file_icon_rect, state, self.scale, theme);
            }
            // Label centred under the tile, truncated to the cell width.
            let ly = icon_y + preview_px + 8.0 * self.scale;
            if self.renaming == Some(i) {
                // Publish the label field's rect and omit the label; the host
                // draws the in-place rename editor here.
                if let Some(sink) = &self.rename_rect {
                    sink.set(Some(Rect::new(
                        cell.x0 + 6.0,
                        ly - 4.0,
                        cell.x1 - 6.0,
                        ly + f64::from(size) + 6.0,
                    )));
                }
            } else {
                let label = truncate(tr, &entry.name, size, self.cell_w() - 10.0);
                let (lw, _) = tr.measure(&label, size, None);
                let lx = cell.x0 + (self.cell_w() - f64::from(lw)) / 2.0;
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
        // `new_leaf` only fails on tree-internal bookkeeping errors, never on
        // caller input; there is none to report here.
        #[allow(
            clippy::unwrap_used,
            reason = "new_leaf cannot fail for a freshly built style"
        )]
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::auto(),
                },
                align_self: Some(taffy::AlignSelf::Start),
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        _tr: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let width = known.width.map_or_else(
            || match available.width {
                taffy::AvailableSpace::Definite(w) => f64::from(w),
                _ => self.cell_w() + 2.0 * self.pad_x(),
            },
            f64::from,
        );
        if self.mode == ViewMode::Grid {
            self.columns = self.columns_for_width(width);
        }
        taffy::Size {
            width: width as f32,
            height: self.content_height() as f32,
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        // `node` was created by `build_layout` in this same tree, so a lookup
        // miss would mean the frame cycle called these out of order.
        #[allow(
            clippy::unwrap_used,
            reason = "node was built in this tree by build_layout; a miss means the frame cycle called these out of order"
        )]
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        if self.mode == ViewMode::Grid {
            self.columns = self.columns_for_width(self.rect.width());
        }
        self.publish_drop_targets();
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
        if self.is_focused
            && let Some(i) = self.selection.primary()
        {
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
                // A held press that travels far enough starts a drag; the app
                // carries the row(s) and, on release, moves them onto a folder
                // (or pins a folder to the sidebar).
                if let Some((start, path, is_dir)) = self.pressed_row.clone()
                    && pt.distance(start) > DRAG_THRESHOLD
                {
                    push(&self.sink, FileCommand::BeginDrag { path, is_dir });
                    self.pressed_row = None;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
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
                    push(
                        &self.sink,
                        FileCommand::Click {
                            index: i,
                            ctrl,
                            shift,
                        },
                    );
                    // A plain press on any row arms a potential drag (move onto a
                    // folder, or pin a folder to the sidebar). A modifier press is
                    // extending the selection, not starting a drag.
                    if !ctrl
                        && !shift
                        && let Some(entry) = self.entries.get(i)
                    {
                        self.pressed_row =
                            Some((self.last_cursor, entry.path.clone(), entry.is_dir));
                        // The app arms the same drag: this view may be
                        // rebuilt before the pointer moves.
                        push(
                            &self.sink,
                            FileCommand::ArmDrag {
                                path: entry.path.clone(),
                                is_dir: entry.is_dir,
                                at: self.last_cursor,
                            },
                        );
                    }
                    *handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                // A release without travel was just a click — disarm the drag.
                self.pressed_row = None;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
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

    fn name(&self) -> &'static str {
        "FileView"
    }
    fn test_id(&self) -> Option<&str> {
        Some("file_view")
    }
    /// Every item by name, so automation aims a drag at the row the
    /// renderer laid out rather than at a guessed offset.
    fn test_rects(&self) -> Vec<(String, Rect)> {
        self.entries
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let rect = match self.mode {
                    ViewMode::List => self.list_row_rect(i),
                    ViewMode::Grid => self.cell_rect(i),
                };
                (format!("file_row:{}", e.name), rect)
            })
            .collect()
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn is_focusable(&self) -> bool {
        true
    }
    fn set_focus(&mut self, f: bool) {
        self.is_focused = f;
    }
    fn is_focused(&self) -> bool {
        self.is_focused
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let primary = self.selection.primary();
        let value = primary
            .and_then(|i| self.entries.get(i))
            .map(|e| e.name.clone());
        let children = self
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                AccessNode::new(Role::ListItem)
                    .with_name(entry.name.clone())
                    .with_description(accessible_entry_description(entry))
                    .with_position(index + 1, self.entries.len())
                    .with_state(AccessState {
                        selected: self.selection.is_selected(index),
                        ..Default::default()
                    })
            })
            .collect();
        let mut node = AccessNode::new(Role::List)
            .with_name(t("files-a11y-list"))
            .with_optional_value(value)
            .with_state(AccessState {
                focusable: true,
                focused: self.is_focused,
                ..Default::default()
            })
            .with_children(children);
        if let Some(i) = primary
            && i < self.entries.len()
        {
            node = node.with_position(i + 1, self.entries.len());
        }
        Some(node)
    }
}

/// Concise row metadata for non-visual navigation. The badge glyphs communicate
/// all of these facts visually; keeping the same state in the semantic row
/// prevents a screen-reader user from having to open Properties for every file.
fn accessible_entry_description(entry: &Entry) -> String {
    let mut facts = Vec::new();
    if entry.is_dir {
        facts.push(t("files-a11y-folder"));
    } else {
        facts.push(t_args("files-a11y-file", &[arg("size", human_size(entry))]));
    }

    if let Some(availability) = &entry.availability {
        use crate::model::{AvailabilityPolicy, ReplicaEvidence, Residency, TransferPhase};
        if availability.transfer.last_error.is_some() {
            facts.push(t("files-a11y-transfer-failed"));
        } else {
            match availability.transfer.phase {
                TransferPhase::Queued => facts.push(t("files-a11y-transfer-queued")),
                TransferPhase::Downloading => facts.push(t("files-a11y-downloading")),
                TransferPhase::Uploading => facts.push(t("files-a11y-uploading")),
                TransferPhase::Verifying => facts.push(t("files-a11y-verifying")),
                TransferPhase::Idle => match availability.residency {
                    Residency::NotResident => facts.push(t("files-a11y-online-only")),
                    Residency::Partial { .. } => facts.push(t("files-a11y-partly-offline")),
                    Residency::Resident { .. } => facts.push(t("files-a11y-available-offline")),
                },
            }
        }
        if availability.policy == AvailabilityPolicy::Pinned {
            facts.push(if availability.policy_inherited {
                t("files-a11y-kept-offline-parent")
            } else {
                t("files-a11y-kept-offline")
            });
        }
        match availability.replicas {
            ReplicaEvidence::Unknown => facts.push(t("files-a11y-replica-unknown")),
            ReplicaEvidence::Known { durable, .. } if durable > 0 => {
                facts.push(t("files-a11y-durably-replicated"));
            }
            ReplicaEvidence::Known { .. } => facts.push(t("files-a11y-not-durably-replicated")),
        }
    } else if let Some(sync) = entry.sync_state {
        facts.push(match sync {
            SyncState::Local => t("files-a11y-available-offline"),
            SyncState::Projected => t("files-a11y-online-only"),
            SyncState::Downloading => t("files-a11y-downloading"),
        });
    }

    if let Some(share) = &entry.share {
        let active = share.active_grants();
        facts.push(t_args(
            "files-a11y-shared-recipients",
            &[arg("count", i64::try_from(active).unwrap_or(i64::MAX))],
        ));
    }
    if entry.is_conflict() {
        facts.push(t("files-a11y-version-conflict"));
    }
    facts.join(". ")
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

fn availability_badge(state: &AvailabilityProjection, theme: &Theme) -> (Icon, Color) {
    let accent = theme.tokens.colors.accent.stops[ui::tokens::colors::ACCENT_PRIMARY_IDX];
    if state.transfer.last_error.is_some() {
        return (Icon::Ban, accent);
    }
    match state.transfer.phase {
        TransferPhase::Queued | TransferPhase::Verifying => return (Icon::Refresh, accent),
        TransferPhase::Downloading => return (Icon::Download, accent),
        TransferPhase::Uploading => return (Icon::Send, accent),
        TransferPhase::Idle => {}
    }
    match state.residency {
        Residency::NotResident => (Icon::CloudDownload, theme.tokens.colors.text_dim),
        Residency::Partial { .. } => (Icon::Download, theme.tokens.colors.text_dim),
        // The star means the keep-offline promise is actually satisfied. A
        // desired pin that is still remote/partial is represented by its real
        // transfer/residency state above, never as a completed outcome.
        Residency::Resident { .. } if state.policy == AvailabilityPolicy::Pinned => {
            (Icon::Star, accent)
        }
        Residency::Resident { .. } if state.safely_replicated() => {
            (Icon::Shield, theme.tokens.colors.text_dim)
        }
        Residency::Resident { .. } => match state.replicas {
            ReplicaEvidence::Unknown => (Icon::Info, theme.tokens.colors.text_dim),
            ReplicaEvidence::Known { .. } => (Icon::Info, accent),
        },
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
    draw_badge(scene, transform, icon_rect, icon, color, scale, theme);
}

fn draw_badge(
    scene: &mut Scene,
    transform: Affine,
    icon_rect: Rect,
    icon: Icon,
    color: Color,
    scale: f64,
    theme: &Theme,
) {
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
    let br = Rect::new(
        cx - bsz / 2.0,
        cy - bsz / 2.0,
        cx + bsz / 2.0,
        cy + bsz / 2.0,
    );
    draw_icon(
        scene,
        transform,
        br,
        icon,
        color,
        LUCIDE_DEFAULT_STROKE_PX * scale.max(1.0),
    );
}

fn draw_availability_badge(
    scene: &mut Scene,
    transform: Affine,
    icon_rect: Rect,
    state: &AvailabilityProjection,
    scale: f64,
    theme: &Theme,
) {
    let (icon, color) = availability_badge(state, theme);
    draw_badge(scene, transform, icon_rect, icon, color, scale, theme);
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

/// The uppercased extension for a type badge (at most 4 chars, so `.markdown`
/// shows `MARK` rather than overflowing). Empty for an extensionless file.
fn ext_label(path: &std::path::Path) -> String {
    match path.extension().and_then(|e| e.to_str()) {
        Some(e) if !e.is_empty() => {
            let up = e.to_ascii_uppercase();
            up.chars().take(4).collect()
        }
        _ => String::new(),
    }
}

/// A stable, mid-tone accent per file category for the type badge. Chosen to
/// stay legible on both light and dark surfaces (the badge fills a translucent
/// tint of it and draws the glyph + extension in the full colour).
fn category_tint(cat: FileCategory) -> Color {
    let [r, g, b] = match cat {
        FileCategory::Image => [0x3B, 0xA7, 0x76],
        FileCategory::Video => [0x8B, 0x5C, 0xF6],
        FileCategory::Audio => [0xE0, 0x55, 0x9A],
        FileCategory::Pdf => [0xE5, 0x48, 0x4D],
        FileCategory::Ebook => [0xD0, 0x8A, 0x2E],
        FileCategory::Document => [0x3E, 0x7B, 0xEA],
        FileCategory::Spreadsheet => [0x2F, 0xA3, 0x6B],
        FileCategory::Presentation => [0xE8, 0x70, 0x3A],
        FileCategory::Archive => [0xB0, 0x7B, 0x3E],
        FileCategory::Code => [0x2A, 0xA0, 0xB4],
        FileCategory::Text => [0x7A, 0x82, 0x90],
        FileCategory::Font => [0x7C, 0x6C, 0xF0],
        FileCategory::Unknown => [0x8A, 0x93, 0xA0],
    };
    Color::from_rgba8(r, g, b, 255)
}

/// Trim `text` with an ellipsis until it fits `max_w`.
fn truncate(tr: &mut TextRenderer, text: &str, size: f32, max_w: f64) -> String {
    if max_w <= 0.0 {
        return String::new();
    }
    let (w, _) = tr.measure(text, size, None);
    if f64::from(w) <= max_w {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut keep = chars.len();
    while keep > 0 {
        keep -= 1;
        let candidate: String = chars
            .get(..keep)
            .map_or_else(String::new, |s| s.iter().collect())
            + "\u{2026}";
        let (cw, _) = tr.measure(&candidate, size, None);
        if f64::from(cw) <= max_w {
            return candidate;
        }
    }
    "\u{2026}".to_string()
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::*;
    use crate::command::{FileCommand, new_sink};
    use std::path::PathBuf;

    fn dev() -> winit::event::DeviceId {
        unsafe { std::mem::zeroed() }
    }

    fn dispatch(v: &mut FileView, tree: &TaffyTree<()>, node: NodeId, ev: &WindowEvent) {
        let mut handled = false;
        let mut focus = None;
        let mut doc = automerge::AutoCommit::new();
        let root = automerge::ROOT;
        let mods = winit::event::Modifiers::default();
        v.handle_event(
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

    fn ent(name: &str, is_dir: bool) -> Entry {
        Entry {
            name: name.into(),
            path: PathBuf::from("/x").join(name),
            is_dir,
            size: 10,
            modified: None,
            mime: String::new(),
            sync_state: None,
            content_hash: None,
            record_id: None,
            availability: None,
            share: None,
            history: None,
            rich_metadata: None,
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
    fn accessibility_exposes_each_file_selection_and_collaboration_state() {
        use ui::widget::Widget;

        let sink = new_sink();
        let mut selection = SelectionModel::new();
        selection.select_only(1);
        let mut entries = vec![ent("folder", true), ent("report.pdf", false)];
        entries[1].size = 4_000;
        entries[1].availability = Some(availability_with(
            "idle",
            &serde_json::json!({"state": "resident", "bytes": 4000}),
            "pinned",
            &serde_json::json!({"state": "known", "reachable": 2, "durable": 1}),
            false,
        ));
        entries[1].share = crate::model::ShareProjection::parse(
            &serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "record_id": uuid::Uuid::nil().to_string(),
                "observed_at_ms": 1,
                "grants": [{
                    "grant_id": uuid::Uuid::nil().to_string(),
                    "subject": {"kind": "bearer_link"},
                    "permission": "View",
                    "expires_at_ms": null,
                    "revoked": false,
                    "expired": false,
                    "used": false,
                    "exhausted": false,
                    "status": "active",
                    "redemptions": 0,
                    "max_redemptions": 1
                }]
            }))
            .unwrap(),
        );

        let view = FileView::new(&entries, &selection, &sink);
        let node = Widget::accessibility(&view).expect("file view has semantics");
        assert_eq!(node.name.as_deref(), Some("Files"));
        assert_eq!(node.children.len(), 2);
        assert_eq!(node.children[0].name.as_deref(), Some("folder"));
        assert!(!node.children[0].state.selected);
        assert_eq!(node.children[1].name.as_deref(), Some("report.pdf"));
        assert!(node.children[1].state.selected);
        assert_eq!(node.children[1].pos_in_set, Some(2));
        assert_eq!(node.children[1].set_size, Some(2));
        let description = node.children[1].description.as_deref().unwrap_or_default();
        for fact in [
            "File, 4\u{a0}kB",
            "Available offline",
            "Kept offline",
            "Durably replicated",
            "Shared with 1 recipient",
        ] {
            assert!(
                description.contains(fact),
                "missing `{fact}` in {description:?}"
            );
        }
    }

    #[test]
    fn grid_flows_left_to_right_and_wraps_to_fill_width() {
        // A wide pane must lay several columns per row, not one item per line.
        let v = grid(12, 900.0);
        assert!(
            v.columns >= 5,
            "wide grid uses many columns, got {}",
            v.columns
        );
        // Item 0 and item 1 sit on the SAME row (item 1 is to the right of 0),
        // proving row-major flow rather than a vertical stack.
        let c0 = v.cell_rect(0);
        let c1 = v.cell_rect(1);
        assert!((c0.y0 - c1.y0).abs() < 1e-6, "items 0 and 1 share a row");
        assert!(c1.x0 > c0.x0, "item 1 is to the right of item 0");
        // The item after a full row wraps down to the next row at the left.
        let wrap = v.cell_rect(v.columns);
        assert!(
            wrap.y0 > c0.y0,
            "item after a full row wraps to the next row"
        );
        assert!(
            (wrap.x0 - c0.x0).abs() < 1e-6,
            "wrapped item returns to the left edge"
        );
    }

    #[test]
    fn grid_columns_scale_with_width_and_never_zero() {
        assert!(grid(20, 1200.0).columns > grid(20, 400.0).columns);
        assert!(
            grid(20, 10.0).columns >= 1,
            "never collapses below one column"
        );
    }

    #[test]
    fn image_cells_request_previews_through_the_shared_service() {
        // The file view must off-load decoding to the shared thumbnail service,
        // never decode per-app: a request for an image cell schedules work on the
        // service and later reads a real preview back from it.
        let dir = tempfile::tempdir().unwrap();
        let img_path = dir.path().join("photo.png");
        let mut buf = image::RgbaImage::new(80, 60);
        for p in buf.pixels_mut() {
            *p = image::Rgba([10, 120, 200, 255]);
        }
        image::DynamicImage::ImageRgba8(buf)
            .save(&img_path)
            .unwrap();

        let cache = thumbnail::ThumbCache::open(dir.path().join("cache")).unwrap();
        let service = Arc::new(thumbnail::ThumbnailService::new(cache, 2, 128));
        let ctx = ThumbCtx {
            service: service.clone(),
            imgs: Rc::new(RefCell::new(HashMap::new())),
        };
        let entry = Entry {
            name: "photo.png".into(),
            path: img_path.clone(),
            is_dir: false,
            size: std::fs::metadata(&img_path).unwrap().len(),
            modified: None,
            mime: String::new(),
            sync_state: None,
            content_hash: None,
            record_id: None,
            availability: None,
            share: None,
            history: None,
            rich_metadata: None,
        };
        let sink = new_sink();
        let sel = SelectionModel::new();
        let _v = FileView::new(std::slice::from_ref(&entry), &sel, &sink);
        let key = FileView::thumb_key(&ctx, &entry);

        // First look schedules generation on the shared service.
        assert!(matches!(
            FileView::request_thumb(&ctx, &entry, key, thumbnail::FileCategory::Image),
            ThumbState::Loading
        ));
        assert_eq!(
            service.pending(),
            1,
            "decode is off-loaded to the shared service"
        );

        service.block_until_idle(std::time::Duration::from_secs(5));
        assert!(
            matches!(
                FileView::request_thumb(&ctx, &entry, key, thumbnail::FileCategory::Image),
                ThumbState::Ready(_)
            ),
            "the service produced a real preview the cell reads back"
        );
    }

    #[test]
    fn workspace_thumbnail_keys_follow_content_across_renames() {
        let dir = tempfile::tempdir().unwrap();
        let (_service, ctx) = thumb_ctx(dir.path());
        let mut a = ent("before.png", false);
        a.path = PathBuf::from("/var/workspace/before.png");
        a.content_hash = Some([0x44; 32]);
        let mut b = a.clone();
        b.name = "after.png".into();
        b.path = PathBuf::from("/var/workspace/after.png");
        b.modified = Some(chrono::Local::now());
        assert_eq!(
            FileView::thumb_key(&ctx, &a),
            FileView::thumb_key(&ctx, &b),
            "the same content reuses its preview after a path/mtime change"
        );
    }

    /// A small PNG on disk, as an `Entry` with no sync state (an ordinary
    /// local file, or a workspace root the driver never annotated). Bigger
    /// than the host-provided preview used in the tests below but still
    /// small enough to survive the service's cap untouched — so the size of
    /// whatever comes back names which source produced it.
    fn photo_on_disk(dir: &std::path::Path, name: &str, w: u32, h: u32) -> Entry {
        let path = dir.join(name);
        let mut buf = image::RgbaImage::new(w, h);
        for p in buf.pixels_mut() {
            *p = image::Rgba([10, 120, 200, 255]);
        }
        image::DynamicImage::ImageRgba8(buf).save(&path).unwrap();
        Entry {
            name: name.into(),
            path: path.clone(),
            is_dir: false,
            size: std::fs::metadata(&path).unwrap().len(),
            modified: None,
            mime: String::new(),
            sync_state: None,
            content_hash: None,
            record_id: None,
            availability: None,
            share: None,
            history: None,
            rich_metadata: None,
        }
    }

    fn thumb_ctx(cache_dir: &std::path::Path) -> (Arc<thumbnail::ThumbnailService>, ThumbCtx) {
        let cache = thumbnail::ThumbCache::open(cache_dir).unwrap();
        let service = Arc::new(thumbnail::ThumbnailService::new(cache, 2, 128));
        let ctx = ThumbCtx {
            service: service.clone(),
            imgs: Rc::new(RefCell::new(HashMap::new())),
        };
        (service, ctx)
    }

    #[test]
    fn a_workspace_photo_the_host_never_previewed_is_still_shown() {
        // A workspace root is often an ordinary directory — no FUSE driver
        // behind it, so no `user.qos.thumb` to read and no sync attribute
        // either. The picture is right there on disk, so the row must show the
        // picture and not a type glyph.
        let dir = tempfile::tempdir().unwrap();
        let entry = photo_on_disk(dir.path(), "harbour.png", 80, 60);
        assert_eq!(
            crate::model::read_thumb_xattr(&entry.path),
            None,
            "the premise: the host handed over no preview for it"
        );
        let (service, ctx) = thumb_ctx(&dir.path().join("cache"));
        let sink = new_sink();
        let sel = SelectionModel::new();
        let _v = FileView::new(std::slice::from_ref(&entry), &sel, &sink);
        let key = FileView::thumb_key(&ctx, &entry);

        assert!(matches!(
            FileView::request_thumb_in(&ctx, &entry, key, thumbnail::FileCategory::Image, true),
            ThumbState::Loading
        ));
        service.block_until_idle(std::time::Duration::from_secs(5));
        let state =
            FileView::request_thumb_in(&ctx, &entry, key, thumbnail::FileCategory::Image, true);
        let ready = state
            .ready()
            .expect("the workspace photo previews from its own bytes");
        assert_eq!(
            (ready.width, ready.height),
            (80, 60),
            "the preview is the file's own picture"
        );
    }

    #[test]
    fn a_host_preview_is_adopted_rather_than_the_file_re_decoded() {
        // When the metadata channel does answer, that answer is what gets
        // painted: the file manager never re-decodes what the host already
        // previewed. The two sources are deliberately different sizes, so the
        // preview that comes back says which one produced it.
        let dir = tempfile::tempdir().unwrap();
        let entry = photo_on_disk(dir.path(), "harbour.png", 80, 60);
        let mut host = image::RgbaImage::new(24, 16);
        for p in host.pixels_mut() {
            *p = image::Rgba([200, 40, 40, 255]);
        }
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(host)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        xattr::set(&entry.path, crate::model::THUMB_XATTR, &png).unwrap();

        let (service, ctx) = thumb_ctx(&dir.path().join("cache"));
        let sink = new_sink();
        let sel = SelectionModel::new();
        let _v = FileView::new(std::slice::from_ref(&entry), &sel, &sink);
        let key = FileView::thumb_key(&ctx, &entry);
        assert!(matches!(
            FileView::request_thumb_in(&ctx, &entry, key, thumbnail::FileCategory::Image, true),
            ThumbState::Loading
        ));
        service.block_until_idle(std::time::Duration::from_secs(5));
        let state =
            FileView::request_thumb_in(&ctx, &entry, key, thumbnail::FileCategory::Image, true);
        let ready = state.ready().expect("the host's preview is adopted");
        assert_eq!(
            (ready.width, ready.height),
            (24, 16),
            "the host's preview was painted, not a fresh decode of the file"
        );
    }

    #[test]
    fn a_projected_file_is_never_hydrated_just_to_preview_it() {
        // Its bytes are on a peer. With no host preview the row keeps its
        // badge rather than pulling the whole file across to draw a small
        // picture of it.
        let dir = tempfile::tempdir().unwrap();
        let mut entry = photo_on_disk(dir.path(), "harbour.png", 80, 60);
        entry.sync_state = Some(crate::model::SyncState::Projected);
        let (service, ctx) = thumb_ctx(&dir.path().join("cache"));
        let sink = new_sink();
        let sel = SelectionModel::new();
        let _v = FileView::new(std::slice::from_ref(&entry), &sel, &sink);
        let key = FileView::thumb_key(&ctx, &entry);
        assert!(matches!(
            FileView::request_thumb_in(&ctx, &entry, key, thumbnail::FileCategory::Image, true),
            ThumbState::Loading
        ));
        service.block_until_idle(std::time::Duration::from_secs(5));
        assert!(matches!(
            FileView::request_thumb_in(&ctx, &entry, key, thumbnail::FileCategory::Image, true),
            ThumbState::Badge(thumbnail::FileCategory::Image)
        ));
        assert_eq!(
            service.pending(),
            0,
            "the metadata-only request settled without decoding the source file"
        );
    }

    #[test]
    fn badge_types_never_schedule_a_decode() {
        let dir = tempfile::tempdir().unwrap();
        let cache = thumbnail::ThumbCache::open(dir.path()).unwrap();
        let service = Arc::new(thumbnail::ThumbnailService::new(cache, 2, 128));
        let ctx = ThumbCtx {
            service: service.clone(),
            imgs: Rc::new(RefCell::new(HashMap::new())),
        };
        let entry = ent("archive.zip", false);
        let sink = new_sink();
        let sel = SelectionModel::new();
        let _v = FileView::new(std::slice::from_ref(&entry), &sel, &sink);
        let key = FileView::thumb_key(&ctx, &entry);
        assert!(matches!(
            FileView::request_thumb(&ctx, &entry, key, thumbnail::FileCategory::Archive),
            ThumbState::Badge(thumbnail::FileCategory::Archive)
        ));
        assert_eq!(service.pending(), 0, "a badge type never schedules work");
    }

    #[test]
    fn zoom_scale_grows_row_height_and_cell_size() {
        let sink = new_sink();
        let sel = SelectionModel::new();
        let base = FileView::new(&items(3), &sel, &sink);
        let zoomed = FileView::new(&items(3), &sel, &sink).with_scale(1.6);
        assert!(zoomed.row_h() > base.row_h(), "zoom grows list row height");
        assert!(
            zoomed.cell_w() > base.cell_w(),
            "zoom grows grid cell width"
        );
        assert!(zoomed.cell_h() > base.cell_h());
    }

    #[test]
    fn narrow_list_collapses_detail_columns_to_give_name_room() {
        let sink = new_sink();
        let sel = SelectionModel::new();
        let mut v = FileView::new(&items(4), &sel, &sink);
        // Wide: all three columns shown.
        v.rect = Rect::new(0.0, 0.0, 900.0, 400.0);
        assert!(
            v.show_size() && v.show_modified(),
            "wide pane shows every column"
        );

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
        assert!(
            name.x1 <= size.x0 && name.x1 > size.x0 - v.pad_x() - 12.0,
            "name reclaims up to the size column"
        );

        // Very narrow: both detail columns collapse; Name spans the whole pane.
        v.rect = Rect::new(0.0, 0.0, 300.0, 400.0);
        assert!(
            !v.show_size() && !v.show_modified(),
            "very narrow shows name only"
        );
        let name = v.header_col_rect(0);
        assert!(
            name.x1 >= v.rect.x1 - v.pad_x() - 1.0,
            "name fills the pane when both details hide"
        );
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
        assert!(v.header_top(vp).abs() < f64::EPSILON);
        // Scrolled: the content is pushed up (rect.y0 < viewport top) so the
        // header pins to the viewport's top edge and stays visible.
        v.rect = Rect::new(0.0, -250.0, 600.0, 550.0);
        assert!(
            v.header_top(vp).abs() < f64::EPSILON,
            "pinned to the viewport top"
        );
        // Without a viewport the header just tracks the content top.
        assert!((v.header_top(None) + 250.0).abs() < f64::EPSILON);
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
        dispatch(
            &mut v,
            &tree,
            node,
            &WindowEvent::CursorMoved {
                device_id: dev(),
                position: (50.0, y).into(),
            },
        );
        dispatch(
            &mut v,
            &tree,
            node,
            &WindowEvent::MouseInput {
                device_id: dev(),
                state: ElementState::Pressed,
                button: MouseButton::Right,
            },
        );

        let cmds = sink.borrow();
        assert!(
            cmds.iter()
                .any(|c| matches!(c, FileCommand::ContextSelect(Some(3)))),
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
        dispatch(
            &mut v,
            &tree,
            node,
            &WindowEvent::CursorMoved {
                device_id: dev(),
                position: (50.0, y).into(),
            },
        );
        dispatch(
            &mut v,
            &tree,
            node,
            &WindowEvent::MouseInput {
                device_id: dev(),
                state: ElementState::Pressed,
                button: MouseButton::Right,
            },
        );

        let cmds = sink.borrow();
        assert!(
            cmds.iter()
                .any(|c| matches!(c, FileCommand::ContextSelect(None))),
            "right-press on empty space clears the selection: {cmds:?}",
        );
    }

    #[test]
    fn sync_badge_maps_each_state_to_a_distinct_emblem() {
        let theme = Theme::dark();
        // Each hydration state gets its own recognisable glyph, and the
        // downloading state is tinted with the accent to catch the eye.
        assert_eq!(sync_badge(SyncState::Local, &theme).0, Icon::Check);
        assert_eq!(
            sync_badge(SyncState::Projected, &theme).0,
            Icon::CloudDownload
        );
        let (dl_icon, dl_color) = sync_badge(SyncState::Downloading, &theme);
        assert_eq!(dl_icon, Icon::Download);
        assert_eq!(
            dl_color,
            theme.tokens.colors.accent.stops[ui::tokens::colors::ACCENT_PRIMARY_IDX],
            "downloading badge uses the accent colour",
        );
    }

    fn availability_with(
        phase: &str,
        residency: &serde_json::Value,
        policy: &str,
        replicas: &serde_json::Value,
        error: bool,
    ) -> AvailabilityProjection {
        let value = serde_json::json!({
            "schema_version": 1,
            "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
            "current_version": "a0d4e23f-c7cb-4ded-a427-b32cf9da9342",
            "residency": residency,
            "transfer": {
                "phase": phase,
                "progress": null,
                "last_error": if error { Some("network") } else { None },
                "last_error_at_ms": if error { Some(7) } else { None },
                "error_truncated": false
            },
            "policy": policy,
            "replicas": replicas,
            "freshness": {
                "metadata_observed_at_ms": 1,
                "local_verified_at_ms": 2,
                "replicas_observed_at_ms": 3
            }
        });
        AvailabilityProjection::parse(&serde_json::to_vec(&value).unwrap()).unwrap()
    }

    #[test]
    fn rich_availability_badges_are_honest_and_actionable() {
        let theme = Theme::dark();
        let unknown = availability_with(
            "idle",
            &serde_json::json!({"state": "resident", "bytes": 1}),
            "automatic",
            &serde_json::json!({"state": "unknown"}),
            false,
        );
        let zero = availability_with(
            "idle",
            &serde_json::json!({"state": "resident", "bytes": 1}),
            "automatic",
            &serde_json::json!({"state": "known", "reachable": 0, "durable": 0}),
            false,
        );
        let safe = availability_with(
            "idle",
            &serde_json::json!({"state": "resident", "bytes": 1}),
            "automatic",
            &serde_json::json!({"state": "known", "reachable": 1, "durable": 1}),
            false,
        );
        assert_eq!(availability_badge(&unknown, &theme).0, Icon::Info);
        assert_eq!(availability_badge(&zero, &theme).0, Icon::Info);
        assert_ne!(
            availability_badge(&unknown, &theme).1,
            availability_badge(&zero, &theme).1,
            "unknown and known-zero replica evidence must remain distinct"
        );
        assert_eq!(availability_badge(&safe, &theme).0, Icon::Shield);

        let downloading = availability_with(
            "downloading",
            &serde_json::json!({"state": "partial", "resident_bytes": 1}),
            "automatic",
            &serde_json::json!({"state": "unknown"}),
            false,
        );
        assert_eq!(availability_badge(&downloading, &theme).0, Icon::Download);
        let pinned = availability_with(
            "idle",
            &serde_json::json!({"state": "resident", "bytes": 1}),
            "pinned",
            &serde_json::json!({"state": "unknown"}),
            false,
        );
        assert_eq!(availability_badge(&pinned, &theme).0, Icon::Star);
        let pending_pin = availability_with(
            "idle",
            &serde_json::json!({"state": "not_resident"}),
            "pinned",
            &serde_json::json!({"state": "unknown"}),
            false,
        );
        assert_eq!(
            availability_badge(&pending_pin, &theme).0,
            Icon::CloudDownload,
            "pin intent must not masquerade as verified local residency"
        );
        let failed = availability_with(
            "idle",
            &serde_json::json!({"state": "not_resident"}),
            "automatic",
            &serde_json::json!({"state": "unknown"}),
            true,
        );
        assert_eq!(availability_badge(&failed, &theme).0, Icon::Ban);
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
        dispatch(
            &mut v,
            &tree,
            node,
            &WindowEvent::CursorMoved {
                device_id: dev(),
                position: (50.0, y).into(),
            },
        );
        dispatch(
            &mut v,
            &tree,
            node,
            &WindowEvent::MouseInput {
                device_id: dev(),
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
        );
        dispatch(
            &mut v,
            &tree,
            node,
            &WindowEvent::CursorMoved {
                device_id: dev(),
                position: (120.0, y).into(),
            },
        );

        let cmds = sink.borrow();
        assert!(
            cmds.iter().any(|c| matches!(c, FileCommand::BeginDrag { path, is_dir: true } if path.ends_with("folder"))),
            "a folder press that travels begins a pin drag: {cmds:?}",
        );
    }

    #[test]
    fn a_file_row_press_begins_a_move_drag() {
        // Files (not only folders) are draggable now — a row drag moves the
        // file into a folder it is dropped onto. The host decides drop
        // semantics; the view just reports the drag started on this row.
        let sink = new_sink();
        let sel = SelectionModel::new();
        let mut v = FileView::new(&items(5), &sel, &sink);
        v.rect = Rect::new(0.0, 0.0, 600.0, 600.0);
        let mut tree = TaffyTree::<()>::new();
        let node = tree.new_leaf(Style::default()).unwrap();
        let y = v.rect.y0 + v.header_h() + v.row_h() / 2.0;
        dispatch(
            &mut v,
            &tree,
            node,
            &WindowEvent::CursorMoved {
                device_id: dev(),
                position: (50.0, y).into(),
            },
        );
        dispatch(
            &mut v,
            &tree,
            node,
            &WindowEvent::MouseInput {
                device_id: dev(),
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
        );
        dispatch(
            &mut v,
            &tree,
            node,
            &WindowEvent::CursorMoved {
                device_id: dev(),
                position: (120.0, y).into(),
            },
        );
        let cmds = sink.borrow();
        assert!(
            cmds.iter().any(|c| matches!(c, FileCommand::BeginDrag { path, is_dir: false } if path.ends_with("f0"))),
            "a file row that travels begins a drag: {cmds:?}",
        );
    }

    #[test]
    fn apply_layout_publishes_folder_rows_as_drop_targets() {
        // The host resolves a drop against the folder rects the view publishes,
        // so only directories must appear, at their laid-out positions.
        let sink = new_sink();
        let sel = SelectionModel::new();
        let mut entries = items(5); // f0..f4 are files
        entries[1] = ent("docs", true);
        entries[3] = ent("music", true);
        let targets: super::DropTargets = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut v = FileView::new(&entries, &sel, &sink).with_drop_targets(targets.clone());
        let mut tree = TaffyTree::<()>::new();
        let node = tree
            .new_leaf(taffy::Style {
                size: taffy::Size {
                    width: Dimension::length(600.0),
                    height: Dimension::length(600.0),
                },
                ..Default::default()
            })
            .unwrap();
        tree.compute_layout(
            node,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(600.0),
                height: taffy::AvailableSpace::Definite(600.0),
            },
        )
        .unwrap();
        v.apply_layout(&tree, node, Point::ZERO);
        let published = targets.borrow();
        let names: Vec<String> = published
            .iter()
            .map(|t| t.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec!["docs".to_string(), "music".to_string()],
            "only folders, in row order"
        );
        // Each target's rect sits on its row (row 1 above row 3).
        assert!(
            published[0].rect.y0 < published[1].rect.y0,
            "row order preserved in geometry"
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
