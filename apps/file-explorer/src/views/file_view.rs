//! Files' content pane: the list or the grid of the folder shown, painting its
//! rows and reading the pointer against them.

#[cfg(test)]
#[cfg(unix)]
use host_fs::attributes as xattr;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use taffy::{Dimension, LengthPercentage, NodeId, Size, Style, TaffyTree};
use ui::widget::TaffyTreeExt;
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect};
use vello::peniko::{Color, Fill, ImageData as PenikoImage};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use thumbnail::{FileCategory, ThumbKey, ThumbState, ThumbnailService, category_for_path};

use super::file_list::{ListInputs, ListSlot, sort_column};
use crate::command::{CommandSink, FileCommand, push};
use crate::i18n::{arg, t, t_args};
use crate::model::{Entry, SortBy, SortOrder, ViewMode, human_size};
use fs_protocol::AvailabilityPolicy;
use fs_protocol::AvailabilityProjectionV1;
use fs_protocol::ProjectedReplicaEvidence;
use fs_protocol::Residency;
use fs_protocol::SyncState;
use fs_protocol::TransferPhase;
use ui::SelectionModel;
use ui::chrome::{SelectionState, paint_row_selection};
use ui::icons::{Icon, LUCIDE_DEFAULT_STROKE_PX, draw_icon, icon_for_path};
use ui::own_paint::OwnPaint;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{MediaArt, MediaCardFace, Widget};

#[derive(Clone)]
pub struct ThumbCtx {
    pub service: Arc<ThumbnailService>,
    pub imgs: Rc<RefCell<HashMap<ThumbKey, PenikoImage>>>,
}

/// Cells painted beyond the viewport, so a partly visible boundary item still draws.
const VISIBLE_RANGE_OVERDRAW_ROWS: usize = 1;
const GRID_LABEL_LINES: usize = 2;

#[derive(Clone, Copy, Debug)]
pub(super) struct Geo {
    pub(super) row: f64,
    pub(super) icon: f64,
    pub(super) grid_top: f64,
    pub(super) icon_label_gap: f64,
    pub(super) icon_lead: f64,
    pub(super) size_col: f64,
    pub(super) modified_col: f64,
    pub(super) date_col: f64,
    pub(super) cell_w: f64,
    pub(super) label_line: f64,
    pub(super) grid_icon: f64,
    pub(super) grid_gap: f64,
    pub(super) scrollbar_gutter: f64,
    pub(super) cell_pad: f64,
    pub(super) label_gap: f64,
    pub(super) item_radius: f64,
}

impl Geo {
    pub(super) fn of(theme: &Theme) -> Self {
        let t = &theme.tokens;
        let sp = &t.spacing;
        let s = |v: f32| f64::from(v);
        let body = s(t.typography.style(TextRole::Body).line_height_px());
        let pointer = s(t.metrics.min_pointer_target);
        Self {
            row: pointer.max(body + s(sp.s2)),
            icon: s(t.metrics.icon_md),
            grid_top: s(sp.s3),
            icon_label_gap: s(sp.s3),
            icon_lead: s(sp.s1),
            size_col: s(sp.s6) * 4.0,
            modified_col: s(sp.s6) * 7.5,
            date_col: s(sp.s6) * 4.75,
            cell_w: s(sp.s4) * 8.0,
            label_line: body,
            grid_icon: s(sp.s12),
            grid_gap: s(sp.s4),
            scrollbar_gutter: s(t.metrics.scrollbar_gutter),
            cell_pad: s(sp.s3),
            label_gap: s(sp.s2),
            item_radius: s(t.radii.item),
        }
    }
}

#[derive(Clone, Debug)]
pub struct FolderDropTarget {
    pub path: PathBuf,
    pub rect: Rect,
}

pub type DropTargets = Rc<RefCell<Vec<FolderDropTarget>>>;

pub type ItemRects = Rc<RefCell<Vec<(usize, Rect)>>>;

pub struct FileView {
    entries: Vec<Entry>,
    selection: SelectionModel,
    mode: ViewMode,
    scale: f64,
    /// Accessibility text scale, apart from item zoom: it widens or drops metadata columns.
    text_scale: f64,
    geo: Geo,
    sort_by: SortBy,
    sort_order: SortOrder,
    sink: CommandSink,

    rect: Rect,
    columns: usize,
    hover: Option<usize>,
    last_cursor: Point,
    is_focused: bool,
    renaming: Option<usize>,
    rename_rect: Option<Rc<Cell<Option<Rect>>>>,
    last_vp: Option<Rect>,
    pressed_row: Option<(Point, PathBuf, bool)>,
    drop_targets: Option<DropTargets>,
    item_rects: Option<ItemRects>,
    thumbs: Option<ThumbCtx>,
    folder_counts: Option<Arc<crate::folder_counts::FolderCounts>>,
    show_hidden: bool,
    workspace_roots: Rc<[PathBuf]>,
    list: ListSlot,
    bottom_inset: f64,
    due: Due,
    own: OwnPaint,
}

#[derive(Default)]
struct Due {
    layout: bool,
    paint: bool,
}

#[must_use]
pub fn drag_threshold() -> f64 {
    f64::from(ui::tokens::spacing::Spacing::BASE.s1)
}

impl FileView {
    pub fn new(entries: &[Entry], selection: &SelectionModel, sink: &CommandSink) -> Self {
        Self {
            due: Due {
                layout: false,
                paint: true,
            },
            entries: entries.to_vec(),
            selection: selection.clone(),
            mode: ViewMode::List,
            scale: 1.0,
            text_scale: 1.0,
            geo: Geo::of(&Theme::dark()),
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
            item_rects: None,
            thumbs: None,
            folder_counts: None,
            show_hidden: false,
            workspace_roots: Rc::from([]),
            list: ListSlot::default(),
            bottom_inset: 0.0,
            own: OwnPaint::default(),
        }
    }

    #[must_use]
    pub fn with_drop_targets(mut self, sink: DropTargets) -> Self {
        self.drop_targets = Some(sink);
        self
    }

    #[must_use]
    pub fn with_item_rects(mut self, sink: ItemRects) -> Self {
        self.item_rects = Some(sink);
        self
    }

    #[must_use]
    pub fn with_thumbs(mut self, thumbs: Option<ThumbCtx>) -> Self {
        self.thumbs = thumbs;
        self
    }

    #[must_use]
    pub fn with_folder_counts(
        mut self,
        counts: Option<Arc<crate::folder_counts::FolderCounts>>,
        show_hidden: bool,
    ) -> Self {
        self.folder_counts = counts;
        self.show_hidden = show_hidden;
        self
    }

    #[must_use]
    pub fn with_list(mut self, slot: ListSlot) -> Self {
        self.list = slot;
        self
    }

    #[must_use]
    pub fn with_bottom_inset(mut self, inset: f64) -> Self {
        self.bottom_inset = inset.max(0.0);
        self
    }

    #[must_use]
    pub fn mode(mut self, mode: ViewMode) -> Self {
        self.mode = mode;
        self
    }

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
    #[must_use]
    pub fn with_theme(mut self, theme: &Theme) -> Self {
        self.geo = Geo::of(theme);
        self.text_scale = f64::from(theme.text_scale);
        self
    }

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

    #[cfg(test)]
    fn row_h(&self) -> f64 {
        self.geo.row * self.scale
    }
    fn grid_top(&self) -> f64 {
        self.geo.grid_top
    }
    fn grid_cell_size_for_width(&self, width: f64) -> (f64, f64) {
        let desired_w = self.geo.cell_w * self.scale * self.text_scale;
        let gap = self.grid_gap();
        let inner = width.max(0.0);
        let compact_w = ((inner - gap) / 2.0).max(0.0);
        let cell_w = if desired_w * 2.0 + gap > inner && compact_w >= self.geo.cell_w {
            compact_w
        } else {
            desired_w
        };
        let height = self.geo.cell_pad * self.scale * 2.0
            + self.grid_art_px(cell_w)
            + self.geo.label_gap * self.scale
            + self.geo.label_line * GRID_LABEL_LINES as f64;
        (cell_w, height)
    }
    fn grid_art_px(&self, cell_w: f64) -> f64 {
        (cell_w * 0.62).max(self.geo.grid_icon * self.scale)
    }
    fn cell_w(&self) -> f64 {
        self.grid_cell_size_for_width(self.rect.width()).0
    }
    fn cell_h(&self) -> f64 {
        self.grid_cell_size_for_width(self.rect.width()).1
    }
    fn grid_gap(&self) -> f64 {
        self.geo.grid_gap * self.scale * self.text_scale.sqrt()
    }
    fn body_style(theme: &Theme) -> ui::text_render::TextStyle {
        theme.text(TextRole::Body)
    }

    fn columns_for_width(&self, width: f64) -> usize {
        let inner = width.max(0.0);
        let stride = self.grid_cell_size_for_width(width).0 + self.grid_gap();
        if stride <= 0.0 {
            return 1;
        }
        // The compact tile width is derived from this very sum, so the quotient
        // is exactly a whole number up to rounding: never lose a column to it.
        ((((inner + self.grid_gap()) / stride) + 1e-9).floor() as usize).max(1)
    }

    fn grid_rows(&self, columns: usize) -> usize {
        if self.entries.is_empty() {
            0
        } else {
            self.entries.len().div_ceil(columns.max(1))
        }
    }

    fn grid_key_target(&self, key: NamedKey) -> Option<usize> {
        let len = self.entries.len();
        if len == 0 {
            return None;
        }
        let current = self.selection.primary().unwrap_or(0).min(len - 1);
        let columns = self.columns.max(1);
        let target = match key {
            NamedKey::ArrowLeft => current.saturating_sub(1),
            NamedKey::ArrowRight => (current + 1).min(len - 1),
            NamedKey::ArrowUp => current.saturating_sub(columns),
            NamedKey::ArrowDown => (current + columns).min(len - 1),
            _ => return None,
        };
        Some(target)
    }

    fn content_height(&self, width: f64) -> f64 {
        let cols = self.columns.max(1);
        let rows = self.grid_rows(cols);
        let cell_h = self.grid_cell_size_for_width(width).1;
        self.grid_top() + rows as f64 * (cell_h + self.grid_gap())
    }

    fn cell_rect(&self, i: usize) -> Rect {
        let cols = self.columns.max(1);
        let col = i % cols;
        let row = i / cols;
        let x = self.rect.x0 + col as f64 * (self.cell_w() + self.grid_gap());
        let y = self.rect.y0 + self.grid_top() + row as f64 * (self.cell_h() + self.grid_gap());
        Rect::new(x, y, x + self.cell_w(), y + self.cell_h())
    }

    fn item_rect(&self, i: usize) -> Option<Rect> {
        match self.mode {
            ViewMode::Grid => Some(self.cell_rect(i)),
            ViewMode::List => self
                .list
                .row_rects()
                .into_iter()
                .find_map(|(index, rect)| (index == i).then_some(rect)),
        }
    }

    fn visible_range(&self, vp: Option<Rect>) -> std::ops::Range<usize> {
        let Some(vp) = vp else {
            return 0..self.entries.len();
        };
        let stride = self.cell_h() + self.grid_gap();
        let start_row = ((vp.y0 - self.rect.y0 - self.grid_top()) / stride)
            .floor()
            .max(0.0) as usize;
        let end_row = ((vp.y1 - self.rect.y0 - self.grid_top()) / stride)
            .ceil()
            .max(0.0) as usize;
        let columns = self.columns.max(1);
        start_row
            .saturating_sub(VISIBLE_RANGE_OVERDRAW_ROWS)
            .saturating_mul(columns)
            .min(self.entries.len())
            ..end_row
                .saturating_add(VISIBLE_RANGE_OVERDRAW_ROWS)
                .saturating_mul(columns)
                .min(self.entries.len())
    }

    /// The bottom lane is the host's; a row partly under it still counts as visible.
    fn fits_viewport_bottom(rect: Rect, vp: Option<Rect>) -> bool {
        vp.is_none_or(|viewport| rect.y1 <= viewport.y1)
    }

    fn publish_drop_targets(&self, vp: Option<Rect>) {
        let Some(sink) = &self.drop_targets else {
            return;
        };
        let mut out = Vec::new();
        let mut items = Vec::new();
        match self.mode {
            ViewMode::List => {
                for (index, rect) in self.list.row_rects() {
                    items.push((index, rect));
                    if let Some(entry) = self.entries.get(index)
                        && entry.is_dir
                    {
                        out.push(FolderDropTarget {
                            path: entry.path.clone(),
                            rect,
                        });
                    }
                }
            }
            ViewMode::Grid => {
                let range = self.visible_range(vp);
                for (offset, entry) in self
                    .entries
                    .get(range.clone())
                    .into_iter()
                    .flatten()
                    .enumerate()
                {
                    let rect = self.cell_rect(range.start + offset);
                    if Self::fits_viewport_bottom(rect, vp) {
                        items.push((range.start + offset, rect));
                    }
                    if entry.is_dir && Self::fits_viewport_bottom(rect, vp) {
                        out.push(FolderDropTarget {
                            path: entry.path.clone(),
                            rect,
                        });
                    }
                }
            }
        }
        *sink.borrow_mut() = out;
        if let Some(rects) = &self.item_rects {
            *rects.borrow_mut() = items;
        }
    }

    fn item_at(&self, pt: Point) -> Option<usize> {
        if pt.x < self.rect.x0 || pt.x > self.rect.x1 || pt.y < self.rect.y0 {
            return None;
        }
        match self.mode {
            ViewMode::List => {
                let table = self.list.rect();
                if pt.y < table.y0 + self.list.header_height() {
                    return None;
                }
                self.list
                    .row_rects()
                    .into_iter()
                    .find_map(|(index, rect)| rect.contains(pt).then_some(index))
                    .filter(|index| *index < self.entries.len())
            }
            ViewMode::Grid => {
                let cols = self.columns.max(1);
                let rel_x = pt.x - self.rect.x0;
                let rel_y = pt.y - self.rect.y0 - self.grid_top();
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

    fn paint_selection(
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        rect: Rect,
        selected: bool,
        hovered: bool,
    ) {
        paint_row_selection(
            scene,
            transform,
            rect,
            SelectionState::new(selected, hovered, false),
            theme.tokens.colors.surfaces.content,
            theme,
        );
    }

    fn thumb_key(ctx: &ThumbCtx, entry: &Entry) -> ThumbKey {
        if let Some(hash) = entry.content_hash {
            return ThumbKey::of_hash(hash, ctx.service.edge());
        }
        let mtime_ms = entry
            .modified
            .map_or(0, |t| t.timestamp_millis().max(0) as u64);
        ThumbKey::of_path_meta(&entry.path, mtime_ms, entry.size, ctx.service.edge())
    }

    /// Ask for `entry`'s preview; a workspace file's goes through the host's preview, never this app's.
    fn request_thumb(
        ctx: &ThumbCtx,
        workspace_roots: &[PathBuf],
        entry: &Entry,
        key: ThumbKey,
        category: FileCategory,
    ) -> ThumbState {
        Self::request_thumb_in(
            ctx,
            entry,
            key,
            category,
            crate::model::path_under_any(&entry.path, workspace_roots),
        )
    }

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
            // A projected file may use the host's preview; without one it keeps its badge rather than fetching its bytes.
            ctx.service
                .request_encoded(key, category, move || crate::model::read_thumb_xattr(&path))
        }
    }

    fn thumb_peniko(ctx: &ThumbCtx, key: ThumbKey, rgba: &thumbnail::RgbaThumb) -> PenikoImage {
        if let Some(img) = ctx.imgs.borrow().get(&key) {
            return img.clone();
        }
        let img = ui::raster::rgba8(rgba.rgba.clone(), rgba.width, rgba.height);
        ctx.imgs.borrow_mut().insert(key, img.clone());
        img
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
        let style = Self::body_style(theme);
        let range = self.visible_range(vp);
        for (offset, entry) in self
            .entries
            .get(range.clone())
            .into_iter()
            .flatten()
            .enumerate()
        {
            let i = range.start + offset;
            let cell = self.cell_rect(i);
            if !Self::fits_viewport_bottom(cell, vp) {
                continue;
            }
            let selected = self.selection.is_selected(i);
            let hovered = self.hover == Some(i) && !selected;
            Self::paint_selection(scene, transform, theme, cell, selected, hovered);
            let pad = self.geo.cell_pad * self.scale;
            let slot_px = self.grid_art_px(cell.width());
            let art_x = cell.x0 + (cell.width() - slot_px) / 2.0;
            let slot = Rect::new(
                art_x,
                cell.y0 + pad,
                art_x + slot_px,
                cell.y0 + pad + slot_px,
            );
            let visual = VisualPaint {
                thumbs: self.thumbs.as_ref(),
                workspace_roots: &self.workspace_roots,
                scale: self.scale,
                glyph_px: (self.geo.grid_icon * self.scale).min(slot_px),
                glyph_color: tokens.colors.text_primary,
            };
            let painted = paint_file_visual(scene, transform, tr, theme, &visual, slot, entry);
            paint_entry_emblem(scene, transform, painted, entry, self.scale, theme);
            let label_y = slot.y1 + self.geo.label_gap * self.scale;
            if self.renaming == Some(i) {
                if let Some(sink) = &self.rename_rect {
                    let inset = f64::from(tokens.spacing.s1);
                    sink.set(Some(Rect::new(
                        cell.x0 + inset,
                        label_y - inset,
                        cell.x1 - inset,
                        label_y + self.geo.label_line + inset,
                    )));
                }
                continue;
            }
            let label_w = (cell.width() - 2.0 * f64::from(tokens.spacing.s2)).max(1.0);
            let lines = tr.clamp_to_lines(&entry.name, &style, label_w as f32, GRID_LABEL_LINES);
            let mut y = label_y;
            for line in lines {
                let (lw, lh) = tr.measure_style(&line, None, &style);
                let lx = cell.x0 + (cell.width() - f64::from(lw)) / 2.0;
                tr.draw_style(
                    scene,
                    &line,
                    tokens.colors.text_primary,
                    transform * Affine::translate((lx, y)),
                    None,
                    &style,
                );
                y += f64::from(lh);
            }
        }
    }

    fn press_item(
        &mut self,
        index: usize,
        ctrl: bool,
        shift: bool,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        self.selection.apply_click(index, ctrl, shift);
        *ctx.focus_target = Some(ctx.node_id);
        push(&self.sink, FileCommand::Click { index, ctrl, shift });
        if !ctrl
            && !shift
            && let Some(entry) = self.entries.get(index)
        {
            self.pressed_row = Some((self.last_cursor, entry.path.clone(), entry.is_dir));
            push(
                &self.sink,
                FileCommand::ArmDrag {
                    path: entry.path.clone(),
                    is_dir: entry.is_dir,
                    at: self.last_cursor,
                },
            );
        }
    }
}

pub(super) struct VisualPaint<'a> {
    pub(super) thumbs: Option<&'a ThumbCtx>,
    pub(super) workspace_roots: &'a [PathBuf],
    pub(super) scale: f64,
    pub(super) glyph_px: f64,
    pub(super) glyph_color: Color,
}

pub(super) fn paint_file_visual(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    visual: &VisualPaint<'_>,
    rect: Rect,
    entry: &Entry,
) -> Rect {
    if let Some(painted) = paint_entry_artwork(scene, transform, visual, rect, entry) {
        return painted;
    }
    if !entry.is_dir
        && let Some(ctx) = visual.thumbs
    {
        let category = category_for_path(&entry.path);
        let key = FileView::thumb_key(ctx, entry);
        if let ThumbState::Ready(rgba) =
            FileView::request_thumb(ctx, visual.workspace_roots, entry, key, category)
        {
            let img = FileView::thumb_peniko(ctx, key, &rgba);
            MediaCardFace::new(MediaArt::Image(&img))
                .paint(&mut PaintCtx::new(scene, transform, tr, theme), rect);
            return rect;
        }
    }
    let glyph = Rect::from_center_size(rect.center(), (visual.glyph_px, visual.glyph_px));
    let icon = if crate::applications::is_app(&entry.path) {
        Icon::AppWindow
    } else {
        icon_for_path(&entry.path, entry.is_dir)
    };
    draw_icon(
        scene,
        transform,
        glyph,
        icon,
        visual.glyph_color,
        LUCIDE_DEFAULT_STROKE_PX * visual.scale.max(1.0),
    );
    glyph
}

fn paint_entry_artwork(
    scene: &mut Scene,
    transform: Affine,
    visual: &VisualPaint<'_>,
    rect: Rect,
    entry: &Entry,
) -> Option<Rect> {
    let icon = entry.app.as_ref()?.icon.as_ref()?;
    let ctx = visual.thumbs?;
    let key = ThumbKey::of_path_meta(icon, 0, 0, ctx.service.edge());
    let ThumbState::Ready(rgba) = ctx
        .service
        .request_file(key, FileCategory::Image, icon.clone())
    else {
        return None;
    };
    let img = FileView::thumb_peniko(ctx, key, &rgba);
    let edge = rect.width().min(rect.height());
    let square = Rect::from_center_size(rect.center(), (edge, edge));
    ui::image_store::paint_image_contain(scene, transform, square, &img, 0.0);
    Some(square)
}

pub(super) fn paint_entry_emblem(
    scene: &mut Scene,
    transform: Affine,
    icon_rect: Rect,
    entry: &Entry,
    scale: f64,
    theme: &Theme,
) {
    if let Some(availability) = &entry.availability {
        draw_availability_badge(scene, transform, icon_rect, availability, scale, theme);
    } else if let Some(state) = entry.sync_state {
        draw_sync_badge(scene, transform, icon_rect, state, scale, theme);
    }
}

#[cfg(test)]
thread_local! { static MEASURE_CALLS: Cell<usize> = const { Cell::new(0) }; }

#[cfg(test)]
thread_local! {
    pub(crate) static ACCESSIBILITY_BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl FileView {
    fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        match self.mode {
            ViewMode::List => {
                if !self.entries.is_empty() {
                    self.list.draw(scene, transform, tr, theme, presence);
                    self.publish_drop_targets(None);
                }
            }
            ViewMode::Grid => {
                let vp = presence.viewport.get();
                self.last_vp = vp;
                self.publish_drop_targets(vp);
                self.draw_grid(scene, transform, tr, theme, vp);
            }
        }
        if self.is_focused
            && let Some(i) = self.selection.primary()
            && let Some(item) = self.item_rect(i)
        {
            let ring = match self.mode {
                ViewMode::List => item.inflate(f64::from(theme.tokens.spacing.s2), 0.0),
                ViewMode::Grid => item,
            };
            let visible = match self.mode {
                ViewMode::List => item.y1 > self.list.rect().y0 + self.list.header_height(),
                ViewMode::Grid => !cull(self.last_vp, item),
            };
            if visible {
                ui::widget::paint_focus_ring(scene, transform, ring, self.geo.item_radius, theme);
            }
        }
    }
}

impl Widget for FileView {
    ui::provides!(dyn ui::widget::controls::ActionControls);

    fn is_layout_dirty(&self) -> bool {
        self.mode == ViewMode::List && self.list.is_layout_dirty()
    }

    fn update(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) -> bool {
        if self.mode != ViewMode::List {
            return false;
        }
        self.list.update(state, path)
    }

    fn needs_animation_frame(&self) -> bool {
        self.mode == ViewMode::List && self.list.needs_animation_frame()
    }

    fn wants_pointer_capture(&self) -> bool {
        self.mode == ViewMode::List && self.list.wants_pointer_capture()
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.list.discard_structure_dirty();
        std::mem::take(&mut self.due.layout)
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.text_scale = f64::from(theme.text_scale);
        self.geo = Geo::of(theme);
        if self.mode == ViewMode::List {
            return self.build_list_layout(taffy, theme, node_map);
        }
        let style = Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::auto(),
            },
            align_self: Some(taffy::AlignSelf::Start),
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        tr: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        #[cfg(test)]
        MEASURE_CALLS.with(|count| count.set(count.get() + 1));
        self.text_scale = f64::from(tr.text_scale());
        let width = known.width.map_or_else(
            || match available.width {
                taffy::AvailableSpace::Definite(w) => f64::from(w),
                _ => self.cell_w(),
            },
            f64::from,
        );
        self.columns = self.columns_for_width(width);
        taffy::Size {
            width: width as f32,
            height: self.content_height(width) as f32,
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        if self.mode == ViewMode::Grid {
            self.columns = self.columns_for_width(self.rect.width());
            return;
        }
        if let Ok(children) = taffy.children(node)
            && let Some(&table) = children.first()
        {
            let base = Point::new(self.rect.x0, self.rect.y0);
            if self.list.apply_layout(taffy, table, base) {
                self.due.layout = true;
            }
        }
        self.publish_drop_targets(None);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if let Some(sink) = &self.rename_rect {
            sink.set(None);
        }
        let mut own = std::mem::take(&mut self.own);
        own.record(scene, transform, |scene| {
            self.paint(scene, transform, tr, theme, presence);
            self.rect
        });
        self.own = own;
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        self.own.damage(regions);
    }

    fn mark_drawn(&mut self) {
        self.due.paint = false;
        self.own.present();
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.due.paint = true;
        match event {
            WindowEvent::KeyboardInput {
                event: key_event, ..
            } if self.is_focused
                && self.mode == ViewMode::Grid
                && key_event.state == ElementState::Pressed =>
            {
                if let Key::Named(key) = key_event.logical_key
                    && let Some(index) = self.grid_key_target(key)
                {
                    push(
                        &self.sink,
                        FileCommand::MoveSelection {
                            index,
                            extend: ctx.modifiers.state().shift_key(),
                        },
                    );
                    *ctx.handled = true;
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let pt = ctx.logical(position);
                self.last_cursor = pt;
                if self.mode == ViewMode::List {
                    self.list.handle_event(event, ctx);
                } else {
                    let new_hover = self.item_at(pt);
                    if new_hover != self.hover {
                        self.hover = new_hover;
                    }
                }
                if let Some((start, path, is_dir)) = self.pressed_row.clone()
                    && pt.distance(start) > drag_threshold()
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
                if self.mode == ViewMode::List {
                    let row = self
                        .item_at(self.last_cursor)
                        .filter(|_| !self.list.in_scrollbar(self.last_cursor));
                    if let Some(index) = row {
                        let ctrl = ctx.modifiers.state().control_key()
                            || ctx.modifiers.state().super_key();
                        let shift = ctx.modifiers.state().shift_key();
                        self.press_item(index, ctrl, shift, ctx);
                        self.list.select(&self.selection);
                        *ctx.handled = true;
                    } else {
                        self.list.point_at(self.last_cursor, ctx);
                        let outcome = self.list.handle_event(event, ctx);
                        if let Some(column) = outcome.sort_key.and_then(sort_column) {
                            push(&self.sink, FileCommand::SortColumn(column));
                        }
                    }
                } else if let Some(i) = self.item_at(self.last_cursor) {
                    let ctrl =
                        ctx.modifiers.state().control_key() || ctx.modifiers.state().super_key();
                    let shift = ctx.modifiers.state().shift_key();
                    self.press_item(i, ctrl, shift, ctx);
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                self.pressed_row = None;
                if self.mode == ViewMode::List {
                    self.list.handle_event(event, ctx);
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
                let target = self.item_at(self.last_cursor);
                if let Some(i) = target {
                    if !self.selection.is_selected(i) {
                        self.selection.select_only(i);
                    }
                    *ctx.focus_target = Some(ctx.node_id);
                }
                push(&self.sink, FileCommand::ContextSelect(target));
            }
            WindowEvent::MouseWheel { .. } if self.mode == ViewMode::List => {
                self.list.handle_event(event, ctx);
            }
            _ => {}
        }
    }

    fn needs_redraw(&self) -> bool {
        ui::widget::redraw_or_children(self.due.paint, self)
    }

    fn name(&self) -> &'static str {
        "FileView"
    }
    fn test_id(&self) -> Option<&str> {
        Some("file_view")
    }
    fn test_rects(&self) -> Vec<(String, Rect)> {
        self.entries
            .iter()
            .enumerate()
            .filter_map(|(i, e)| Some((format!("file_row:{}", e.name), self.item_rect(i)?)))
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

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        path: &ui::widget::ModelPath,
    ) -> bool {
        self.mode == ViewMode::List
            && self
                .list
                .perform_semantic_action(target, action, state, path)
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        #[cfg(test)]
        ACCESSIBILITY_BUILDS.with(|count| count.set(count.get() + 1));
        let primary = self.selection.primary();
        if self.mode == ViewMode::List
            && let Some(mut table) = self.list.accessibility()
        {
            for row in &mut table.children {
                let Some(entry) = row
                    .pos_in_set
                    .and_then(|position| position.checked_sub(1))
                    .and_then(|index| self.entries.get(index))
                else {
                    continue;
                };
                row.name = Some(entry.name.clone());
                row.description = Some(accessible_entry_description(entry));
            }
            table.state.focused = self.is_focused;
            return Some(table);
        }
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
                    .with_bounds(self.cell_rect(index))
                    .with_state(AccessState::new().selected(self.selection.is_selected(index)))
            })
            .collect();
        let mut node = AccessNode::new(Role::List)
            .with_name(t("files-a11y-list"))
            .with_bounds(
                self.last_vp
                    .map_or(self.rect, |viewport| self.rect.intersect(viewport)),
            )
            .with_optional_value(value)
            .with_state(AccessState::new().focusable(true).focused(self.is_focused))
            .with_children(children);
        if let Some(i) = primary
            && i < self.entries.len()
        {
            node = node.with_position(i + 1, self.entries.len());
        }
        Some(node)
    }
}

impl FileView {
    fn build_list_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.list.sync(&ListInputs {
            entries: &self.entries,
            selection: &self.selection,
            sort_by: self.sort_by,
            sort_order: self.sort_order,
            scale: self.scale,
            text_scale: self.text_scale,
            geo: self.geo,
            thumbs: self.thumbs.clone(),
            folder_counts: self.folder_counts.clone(),
            show_hidden: self.show_hidden,
            workspace_roots: Rc::clone(&self.workspace_roots),
            renaming: self.renaming,
            rename_rect: self.rename_rect.clone(),
            theme,
        });
        let children: Vec<NodeId> = self.list.build_layout(taffy, theme).into_iter().collect();
        let style = Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: Size::from_percent(1.0, 1.0),
            padding: taffy::Rect {
                left: LengthPercentage::length(0.0),
                right: LengthPercentage::length(0.0),
                top: LengthPercentage::length(0.0),
                bottom: LengthPercentage::length(self.bottom_inset as f32),
            },
            ..Default::default()
        };
        taffy.widget_node(style, &children, node_map, self)
    }
}

impl ui::widget::controls::ActionControls for FileView {
    fn has_action_control_bindings(&self) -> bool {
        false
    }
}

fn accessible_entry_description(entry: &Entry) -> String {
    let mut facts = Vec::new();
    if entry.is_dir {
        facts.push(t("files-a11y-folder"));
    } else {
        facts.push(t_args("files-a11y-file", &[arg("size", human_size(entry))]));
    }

    if let Some(availability) = &entry.availability {
        use fs_protocol::AvailabilityPolicy;
        use fs_protocol::ProjectedReplicaEvidence;
        use fs_protocol::Residency;
        use fs_protocol::TransferPhase;
        if availability.unavailable() {
            facts.push(t("files-a11y-unavailable"));
        } else if availability.transfer.last_error.is_some() {
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
            ProjectedReplicaEvidence::Unknown => facts.push(t("files-a11y-replica-unknown")),
            ProjectedReplicaEvidence::Known { durable, .. } if durable > 0 => {
                facts.push(t("files-a11y-durably-replicated"));
            }
            ProjectedReplicaEvidence::Known { .. } => {
                facts.push(t("files-a11y-not-durably-replicated"));
            }
        }
    } else if let Some(sync) = entry.sync_state {
        facts.push(match sync {
            SyncState::Local => t("files-a11y-available-offline"),
            SyncState::Projected => t("files-a11y-online-only"),
            SyncState::Downloading => t("files-a11y-downloading"),
        });
    }

    if entry.is_conflict() {
        facts.push(t("files-a11y-version-conflict"));
    }
    facts.join(". ")
}

fn sync_badge(state: SyncState, theme: &Theme) -> (Icon, Color) {
    let colors = &theme.tokens.colors;
    match state {
        SyncState::Local => (Icon::Check, colors.text_dim),
        SyncState::Projected => (Icon::CloudDownload, colors.text_dim),
        SyncState::Downloading => (Icon::Download, colors.semantic.info),
    }
}

fn availability_badge(state: &AvailabilityProjectionV1, theme: &Theme) -> (Icon, Color) {
    let colors = &theme.tokens.colors;
    if state.transfer.last_error.is_some() {
        return (Icon::Ban, colors.semantic.danger);
    }
    match state.transfer.phase {
        TransferPhase::Queued | TransferPhase::Verifying => {
            return (Icon::Refresh, colors.semantic.info);
        }
        TransferPhase::Downloading => return (Icon::Download, colors.semantic.info),
        TransferPhase::Uploading => return (Icon::Send, colors.semantic.info),
        TransferPhase::Idle => {}
    }
    match state.residency {
        Residency::NotResident => (Icon::CloudDownload, colors.text_dim),
        Residency::Partial { .. } => (Icon::Download, colors.text_dim),
        // The star says the keep-offline promise is kept: a pin still remote shows its plain state.
        Residency::Resident { .. } if state.policy == AvailabilityPolicy::Pinned => {
            (Icon::Star, colors.text_primary)
        }
        Residency::Resident { .. } if state.safely_replicated() => (Icon::Shield, colors.text_dim),
        Residency::Resident { .. } => match state.replicas {
            ProjectedReplicaEvidence::Unknown => (Icon::Info, colors.text_dim),
            ProjectedReplicaEvidence::Known { .. } => (Icon::Info, colors.semantic.warning),
        },
    }
}

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
    let bsz = (icon_rect.width() * 0.62).clamp(9.0, f64::from(theme.tokens.metrics.icon_md));
    let cx = icon_rect.x1;
    let cy = icon_rect.y1;
    scene.fill(
        Fill::NonZero,
        transform,
        theme.tokens.colors.surfaces.content,
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
    state: &AvailabilityProjectionV1,
    scale: f64,
    theme: &Theme,
) {
    let (icon, color) = availability_badge(state, theme);
    draw_badge(scene, transform, icon_rect, icon, color, scale, theme);
}

fn cull(vp: Option<Rect>, r: Rect) -> bool {
    match vp {
        Some(v) => {
            let margin = f64::from(ui::tokens::spacing::Spacing::BASE.s1);
            r.y1 < v.y0 - margin || r.y0 > v.y1 + margin
        }
        None => false,
    }
}

pub(super) fn truncate(
    tr: &mut TextRenderer,
    text: &str,
    style: &ui::text_render::TextStyle,
    max_w: f64,
) -> String {
    if max_w <= 0.0 {
        return String::new();
    }
    let (w, _) = tr.measure_style(text, None, style);
    if f64::from(w) <= max_w {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut keep = chars.len();
    while keep > 0 {
        keep -= 1;
        // Elide the middle, keeping the extension and the end of the name that tells files apart.
        let suffix = keep.saturating_mul(2).div_ceil(3);
        let prefix = keep.saturating_sub(suffix);
        let candidate = format!(
            "{}…{}",
            chars.iter().take(prefix).collect::<String>(),
            chars
                .iter()
                .skip(chars.len().saturating_sub(suffix))
                .collect::<String>()
        );
        let (cw, _) = tr.measure_style(&candidate, None, style);
        if f64::from(cw) <= max_w {
            return candidate;
        }
    }
    "\u{2026}".to_string()
}

impl FileView {
    #[must_use]
    pub fn with_workspace_roots(mut self, roots: Rc<[PathBuf]>) -> Self {
        self.workspace_roots = roots;
        self
    }
    #[must_use]
    pub fn with_hover(mut self, index: Option<usize>) -> Self {
        self.hover = index;
        self
    }
    pub(crate) fn prefetch_thumbnails(
        ctx: &ThumbCtx,
        entries: &[Entry],
        workspace_roots: &[PathBuf],
    ) {
        for entry in entries.iter().filter(|entry| !entry.is_dir) {
            let key = Self::thumb_key(ctx, entry);
            let in_workspace = crate::model::path_under_any(&entry.path, workspace_roots);
            let _ = Self::request_thumb_in(
                ctx,
                entry,
                key,
                category_for_path(&entry.path),
                in_workspace,
            );
        }
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::super::file_list::shape_columns;
    use super::*;
    use crate::command::{FileCommand, new_sink};
    use std::path::PathBuf;

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
            history: None,
            app: None,
            rich_metadata: None,
            observed: None,
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
    fn scroll_measurement_culling_and_drop_targets_bound_the_work() {
        scroll_layout_reuses_real_file_view_measurement();
        rows_outside_the_scroll_viewport_are_culled();
        visible_range_avoids_scanning_a_large_directory();
        grid_drop_targets_are_limited_to_visible_folders();
        grid_leaves_a_reserved_bottom_lane_clear();
    }

    fn scroll_layout_reuses_real_file_view_measurement() {
        use ui::launch::ShotFrame;
        use ui::widget::Scrollable;
        fn view_rect(scroll: &Scrollable) -> Rect {
            scroll.children()[0].get_rect()
        }
        for mode in [ViewMode::Grid] {
            let sink = new_sink();
            let view = FileView::new(&items(1200), &SelectionModel::new(), &sink).mode(mode);
            let mut scroll = Scrollable::new_vertical().with_child(Box::new(view));
            let theme = Theme::dark();
            let mut frame = ShotFrame::new((1000, 600), theme.clone());
            frame.layout(&mut scroll).unwrap();
            let (_, tree, node) = frame.layout_parts().expect("laid out");
            let (tree, node) = (tree.clone(), node);
            scroll.mark_drawn();
            let before = view_rect(&scroll);
            MEASURE_CALLS.with(|count| count.set(0));
            for _ in 0..4 {
                scroll.scroll_by(0.0, 20.0);
                scroll.apply_layout(&tree, node, Point::ZERO);
                scroll.mark_drawn();
            }
            let after = view_rect(&scroll);
            assert_eq!(after.y0, before.y0 - 80.0);
            assert_eq!(after.size(), before.size());
            assert_eq!(
                MEASURE_CALLS.with(Cell::get),
                0,
                "scrolling an unchanged directory must not remeasure its rows"
            );
            MEASURE_CALLS.with(|count| count.set(0));
            frame.layout_at(&mut scroll, (1200, 600)).unwrap();
            assert!(MEASURE_CALLS.with(Cell::get) > 0);
            let resized = view_rect(&scroll);
            if mode == ViewMode::Grid {
                assert!(resized.height() < before.height());
            }
            let scaled = Theme::dark().with_text_scale(2.0);
            let mut frame = ShotFrame::new((1200, 600), scaled.clone());
            MEASURE_CALLS.with(|count| count.set(0));
            frame.layout(&mut scroll).unwrap();
            assert!(MEASURE_CALLS.with(Cell::get) > 0);
            let scaled_rect = view_rect(&scroll);
            let mut expected = FileView::new(&items(1200), &SelectionModel::new(), &sink)
                .mode(mode)
                .with_theme(&scaled);
            expected.columns = expected.columns_for_width(scaled_rect.width());
            let expected_height = expected.content_height(scaled_rect.width()).round();
            assert!(
                (scaled_rect.height() - expected_height).abs() < 0.01,
                "{mode:?}: scaled height {}, expected {expected_height}, width {}, columns {}",
                scaled_rect.height(),
                scaled_rect.width(),
                expected.columns,
            );
            let mut replaced = Scrollable::new_vertical().with_child(Box::new(
                FileView::new(&items(120), &SelectionModel::new(), &sink).mode(mode),
            ));
            MEASURE_CALLS.with(|count| count.set(0));
            frame.layout(&mut replaced).unwrap();
            assert!(MEASURE_CALLS.with(Cell::get) > 0);
            let replaced_rect = view_rect(&replaced);
            let mut expected = FileView::new(&items(120), &SelectionModel::new(), &sink)
                .mode(mode)
                .with_theme(&scaled);
            expected.columns = expected.columns_for_width(replaced_rect.width());
            assert!(
                (replaced_rect.height() - expected.content_height(replaced_rect.width()).round())
                    .abs()
                    < 0.01
            );
        }
    }

    #[test]
    fn list_geometry_follows_width_zoom_and_text_scale() {
        nested_navigation_layout_preserves_large_text_file_row_geometry();
        truncated_file_names_preserve_distinguishing_suffix();
        neighbouring_faces_keep_a_token_gap();
        grid_zoom_grows_the_cell();
    }

    fn nested_navigation_layout_preserves_large_text_file_row_geometry() {
        let sink = new_sink();
        let view = FileView::new(&items(2), &SelectionModel::new(), &sink);
        let nav = ui::widget::NavShell::new()
            .content_kind(ui::widget::ContentKind::List)
            .sidebar(Box::new(ui::widget::Text::new("Places")))
            .content(Box::new(view));
        let theme = Theme::dark().with_text_scale(2.0);
        let nav = ui::headless::Framed::new(ui::State::new(), nav, (1000, 700), theme.clone());
        let row = nav
            .children()
            .iter()
            .filter_map(|child| child.accessibility())
            .flat_map(|node| node.children)
            .find(|node| node.name.as_deref() == Some("f0"))
            .unwrap();
        let bounds = row.bounds.unwrap();
        let line = f64::from(theme.text(TextRole::Body).line_height_px());
        assert!(
            bounds.y1 - bounds.y0 >= line,
            "nested rows must receive 200% layout scale: {bounds:?}"
        );
        let scene = ui::headless::Framed::harness(&nav).scene();
        let body = theme.text(TextRole::Body);
        assert!(
            scene
                .encoding()
                .resources
                .glyph_runs
                .iter()
                .any(|run| (run.font_size - body.size).abs() < 0.01)
        );
    }

    fn truncated_file_names_preserve_distinguishing_suffix() {
        let mut tr = TextRenderer::new();
        tr.set_text_scale(2.0);
        let style = Theme::dark().text(TextRole::Body);
        let first = truncate(&mut tr, "Document-000.txt", &style, 190.0);
        let next = truncate(&mut tr, "Document-060.txt", &style, 190.0);
        assert_ne!(first, next);
        assert!(next.ends_with("060.txt"), "{next}");
    }

    #[test]
    fn accessibility_and_badges_describe_each_file_state() {
        accessibility_list_bounds_describe_the_scroll_viewport();
        accessibility_exposes_each_file_selection_and_collaboration_state();
        sync_badge_maps_each_state_to_a_distinct_emblem();
        rich_availability_badges_are_honest_and_actionable();
    }

    fn accessibility_list_bounds_describe_the_scroll_viewport() {
        let sink = new_sink();
        let entries = vec![ent("a.txt", false), ent("b.txt", false)];
        let mut view = FileView::new(&entries, &SelectionModel::new(), &sink).mode(ViewMode::Grid);
        view.rect = Rect::new(100.0, -500.0, 700.0, 2500.0);
        let viewport = Rect::new(100.0, 150.0, 700.0, 750.0);
        view.last_vp = Some(viewport);
        assert_eq!(
            view.accessibility()
                .unwrap()
                .bounds
                .map(ui::a11y::AccessBounds::to_rect),
            Some(viewport)
        );
        assert_eq!(view.accessibility().unwrap().children.len(), 2);
    }

    fn accessibility_exposes_each_file_selection_and_collaboration_state() {
        let sink = new_sink();
        let mut selection = SelectionModel::new();
        selection.select_only(1);
        let entries = accessible_entries();
        let mut view = FileView::new(&entries, &selection, &sink).mode(ViewMode::Grid);
        view.rect = Rect::new(0.0, 0.0, 900.0, 600.0);
        view.columns = view.columns_for_width(900.0);
        let node = Widget::accessibility(&view).expect("file view has semantics");
        assert_eq!(node.name.as_deref(), Some("Files"));
        assert_eq!(node.children.len(), 2);
        assert_eq!(node.children[0].name.as_deref(), Some("folder"));
        assert!(!node.children[0].state.selected);
        assert_eq!(node.children[1].name.as_deref(), Some("report.pdf"));
        assert!(node.children[1].state.selected);
        assert_eq!(node.children[1].pos_in_set, Some(2));
        assert_eq!(node.children[1].set_size, Some(2));
        assert_description_has_every_fact(node.children[1].description.as_deref());
    }

    #[test]
    fn grid_flows_wraps_and_navigates_by_visual_rows() {
        grid_flows_left_to_right_and_wraps_to_fill_width();
        grid_columns_scale_with_width_and_never_zero();
        grid_arrow_targets_follow_the_measured_visual_row();
        phone_grid_keeps_two_accessible_columns_at_normal_and_large_zoom();
        grid_labels_share_one_baseline_and_fit_two_lines_inside_the_cell();
        grid_names_wrap_instead_of_being_cut();
    }

    fn grid_flows_left_to_right_and_wraps_to_fill_width() {
        let v = grid(12, 900.0);
        assert!(
            v.columns >= 5,
            "wide grid uses many columns, got {}",
            v.columns
        );
        let c0 = v.cell_rect(0);
        let c1 = v.cell_rect(1);
        assert!((c0.y0 - c1.y0).abs() < 1e-6, "items 0 and 1 share a row");
        assert!(c1.x0 > c0.x0, "item 1 is to the right of item 0");
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

    fn grid_columns_scale_with_width_and_never_zero() {
        assert!(grid(20, 1200.0).columns > grid(20, 400.0).columns);
        assert!(
            grid(20, 10.0).columns >= 1,
            "never collapses below one column"
        );
    }

    fn grid_arrow_targets_follow_the_measured_visual_row() {
        let mut v = grid(8, 400.0);
        v.columns = 3;
        v.selection.select_only(1);
        assert_eq!(v.grid_key_target(NamedKey::ArrowRight), Some(2));
        assert_eq!(v.grid_key_target(NamedKey::ArrowLeft), Some(0));
        assert_eq!(v.grid_key_target(NamedKey::ArrowDown), Some(4));
        assert_eq!(v.grid_key_target(NamedKey::ArrowUp), Some(0));

        v.selection.select_only(7);
        assert_eq!(v.grid_key_target(NamedKey::ArrowDown), Some(7));
        assert_eq!(v.grid_key_target(NamedKey::ArrowRight), Some(7));
    }

    fn phone_grid_keeps_two_accessible_columns_at_normal_and_large_zoom() {
        for scale in [1.0, 1.6] {
            let sink = new_sink();
            let sel = SelectionModel::new();
            let mut v = FileView::new(&items(6), &sel, &sink)
                .mode(ViewMode::Grid)
                .with_scale(scale);
            v.rect = Rect::new(0.0, 0.0, 358.0, 600.0);
            v.columns = v.columns_for_width(v.rect.width());

            assert_eq!(v.columns, 2, "scale {scale} keeps two phone columns");
            let first = v.cell_rect(0);
            let second = v.cell_rect(1);
            assert!(
                first.width() >= 44.0 && first.height() >= 44.0,
                "scale {scale} keeps a usable pointer/AT target: {first:?}"
            );
            assert!(
                (first.y0 - second.y0).abs() < 1e-6 && second.x0 > first.x0,
                "scale {scale} lays the first two entries side-by-side"
            );
            assert_eq!(
                v.item_at(first.center()),
                Some(0),
                "the compact visual bounds remain the pointer target"
            );
            assert_eq!(
                v.item_at(second.center()),
                Some(1),
                "the second compact target remains independently reachable"
            );
            let automation_bounds = v.test_rects();
            assert_eq!(automation_bounds[0].1, first);
            assert_eq!(automation_bounds[1].1, second);

            v.set_focus(true);
            let semantics = Widget::accessibility(&v).expect("grid exposes its list semantics");
            assert!(semantics.state.focusable && semantics.state.focused);
            assert_eq!(
                semantics.children.len(),
                6,
                "all compact targets remain reachable through assistive technology"
            );
        }
    }

    #[test]
    fn thumbnails_use_the_shared_service_or_host_without_hydrating() {
        image_cells_request_previews_through_the_shared_service();
        workspace_thumbnail_keys_follow_content_across_renames();
        a_workspace_photo_the_host_never_previewed_is_still_shown();
        #[cfg(unix)]
        a_host_preview_is_adopted_rather_than_the_file_re_decoded();
        a_projected_file_is_never_hydrated_just_to_preview_it();
        badge_types_never_schedule_a_decode();
    }

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
        let (settled_tx, settled) = std::sync::mpsc::channel();
        service.set_wake(move || {
            settled_tx.send(std::thread::current().id()).unwrap();
        });
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
            history: None,
            app: None,
            rich_metadata: None,
            observed: None,
        };
        let sink = new_sink();
        let sel = SelectionModel::new();
        let _v = FileView::new(std::slice::from_ref(&entry), &sel, &sink);
        let key = FileView::thumb_key(&ctx, &entry);

        assert!(matches!(
            FileView::request_thumb(&ctx, &[], &entry, key, thumbnail::FileCategory::Image),
            ThumbState::Loading
        ));
        let worker = settled.recv_timeout(tilo_test_env::hang_bound()).unwrap();
        assert_ne!(
            worker,
            std::thread::current().id(),
            "decode is off-loaded to a shared-service worker"
        );

        // The wake follows settlement; an instantaneous pending count cannot
        // prove off-loading because a fast worker may already have finished.
        service.block_until_idle(tilo_test_env::hang_bound());
        assert_eq!(service.pending(), 0, "generation settled within the bound");
        assert!(
            service.cache().contains(&key),
            "the shared service cached the preview"
        );
        let shared = service.peek(&key).unwrap();
        let cell = FileView::request_thumb(&ctx, &[], &entry, key, thumbnail::FileCategory::Image);
        assert!(
            matches!(cell, ThumbState::Ready(_)),
            "the service produced a real preview the cell reads back"
        );
        assert!(
            Arc::ptr_eq(shared.ready().unwrap(), cell.ready().unwrap()),
            "the cell reads the service-owned preview rather than decoding its own"
        );
    }

    fn workspace_thumbnail_keys_follow_content_across_renames() {
        let dir = tempfile::tempdir().unwrap();
        let (_service, ctx) = thumb_ctx(dir.path());
        let mut a = ent("before.png", false);
        a.path = PathBuf::from("/var/workspace/before.png");
        a.content_hash = Some([0x44; 32]);
        let mut b = a.clone();
        b.name = "after.png".into();
        b.path = PathBuf::from("/var/workspace/after.png");
        b.modified = Some(ui::clock::local_now());
        assert_eq!(
            FileView::thumb_key(&ctx, &a),
            FileView::thumb_key(&ctx, &b),
            "the same content reuses its preview after a path/mtime change"
        );
    }

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
            history: None,
            app: None,
            rich_metadata: None,
            observed: None,
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

    fn a_workspace_photo_the_host_never_previewed_is_still_shown() {
        // A workspace root is often an ordinary directory — no FUSE driver
        // behind it, so no `user.tilo.thumb` to read and no sync attribute
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
        service.block_until_idle(tilo_test_env::hang_bound());
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

    #[cfg(unix)]
    fn a_host_preview_is_adopted_rather_than_the_file_re_decoded() {
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
        xattr::set(&entry.path, fs_protocol::THUMB_XATTR, &png).unwrap();

        let (service, ctx) = thumb_ctx(&dir.path().join("cache"));
        let sink = new_sink();
        let sel = SelectionModel::new();
        let _v = FileView::new(std::slice::from_ref(&entry), &sel, &sink);
        let key = FileView::thumb_key(&ctx, &entry);
        assert!(matches!(
            FileView::request_thumb_in(&ctx, &entry, key, thumbnail::FileCategory::Image, true),
            ThumbState::Loading
        ));
        service.block_until_idle(tilo_test_env::hang_bound());
        let state =
            FileView::request_thumb_in(&ctx, &entry, key, thumbnail::FileCategory::Image, true);
        let ready = state.ready().expect("the host's preview is adopted");
        assert_eq!(
            (ready.width, ready.height),
            (24, 16),
            "the host's preview was painted, not a fresh decode of the file"
        );
    }

    fn a_projected_file_is_never_hydrated_just_to_preview_it() {
        // Its bytes are on a peer. With no host preview the row keeps its
        // badge rather than pulling the whole file across to draw a small
        // picture of it.
        let dir = tempfile::tempdir().unwrap();
        let mut entry = photo_on_disk(dir.path(), "harbour.png", 80, 60);
        entry.sync_state = Some(fs_protocol::SyncState::Projected);
        let (service, ctx) = thumb_ctx(&dir.path().join("cache"));
        let sink = new_sink();
        let sel = SelectionModel::new();
        let _v = FileView::new(std::slice::from_ref(&entry), &sel, &sink);
        let key = FileView::thumb_key(&ctx, &entry);
        assert!(matches!(
            FileView::request_thumb_in(&ctx, &entry, key, thumbnail::FileCategory::Image, true),
            ThumbState::Loading
        ));
        service.block_until_idle(tilo_test_env::hang_bound());
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
            FileView::request_thumb(&ctx, &[], &entry, key, thumbnail::FileCategory::Archive),
            ThumbState::Badge(thumbnail::FileCategory::Archive)
        ));
        assert_eq!(service.pending(), 0, "a badge type never schedules work");
    }

    fn rows_outside_the_scroll_viewport_are_culled() {
        let vp = Some(Rect::new(0.0, 100.0, 600.0, 400.0));
        assert!(cull(vp, Rect::new(0.0, 0.0, 600.0, 20.0)));
        assert!(cull(vp, Rect::new(0.0, 500.0, 600.0, 520.0)));
        assert!(!cull(vp, Rect::new(0.0, 200.0, 600.0, 220.0)));
        assert!(!cull(vp, Rect::new(0.0, 98.0, 600.0, 102.0)));
        assert!(!cull(None, Rect::new(0.0, 99_999.0, 600.0, 100_000.0)));
    }

    fn visible_range_avoids_scanning_a_large_directory() {
        let sink = new_sink();
        let sel = SelectionModel::new();
        let mut v = FileView::new(&items(100_000), &sel, &sink).mode(ViewMode::Grid);
        v.columns = 5;
        v.rect = Rect::new(0.0, -34_000.0, 800.0, 3_366_000.0);
        let grid = v.visible_range(Some(Rect::new(0.0, 0.0, 800.0, 600.0)));
        assert!(
            grid.start > 1_000,
            "the grid viewport starts well below row zero"
        );
        assert!(
            grid.len() < 60,
            "only visible grid cells plus overdraw are visited"
        );
    }

    fn grid_drop_targets_are_limited_to_visible_folders() {
        let sink = new_sink();
        let sel = SelectionModel::new();
        let entries = (0..2_000)
            .map(|i| {
                let mut entry = items(1).pop().expect("fixture entry");
                entry.is_dir = true;
                entry.name = format!("folder-{i}");
                entry
            })
            .collect::<Vec<_>>();
        let targets = Rc::new(RefCell::new(Vec::new()));
        let mut v = FileView::new(&entries, &sel, &sink)
            .mode(ViewMode::Grid)
            .with_drop_targets(targets.clone());
        v.rect = Rect::new(0.0, -10_000.0, 800.0, 58_000.0);
        v.columns = v.columns_for_width(800.0);
        v.publish_drop_targets(Some(Rect::new(0.0, 0.0, 800.0, 600.0)));
        assert!(
            targets.borrow().len() < 64,
            "the drag registry retains only folders a pointer can reach"
        );
    }

    fn grid_leaves_a_reserved_bottom_lane_clear() {
        let sink = new_sink();
        let sel = SelectionModel::new();
        let mut entries = items(20);
        for entry in &mut entries {
            entry.is_dir = true;
        }
        let targets = Rc::new(RefCell::new(Vec::new()));
        let mut v = FileView::new(&entries, &sel, &sink)
            .mode(ViewMode::Grid)
            .with_drop_targets(Rc::clone(&targets));
        v.rect = Rect::new(0.0, 0.0, 358.0, 1_200.0);
        v.columns = v.columns_for_width(v.rect.width());
        let first_partial = v.cell_rect(v.columns);
        let viewport = Rect::new(
            0.0,
            0.0,
            358.0,
            first_partial.y0 + first_partial.height() / 2.0,
        );
        assert!(
            (first_partial.y0..first_partial.y1).contains(&viewport.y1),
            "fixture has a partially visible cell: {first_partial:?}"
        );
        assert!(
            !FileView::fits_viewport_bottom(first_partial, Some(viewport)),
            "the shared draw guard defers the partial cell"
        );
        v.publish_drop_targets(Some(viewport));
        assert!(
            targets
                .borrow()
                .iter()
                .all(|target| target.rect.y1 <= viewport.y1),
            "no partial folder remains a drop target in the status lane"
        );
    }

    #[test]
    fn pointer_presses_emit_context_select_and_drag_commands() {
        right_press_selects_the_row_under_the_cursor_for_the_menu();
        right_press_on_empty_space_clears_the_selection_for_the_menu();
        dragging_a_directory_row_begins_a_pin_drag();
        a_file_row_press_begins_a_move_drag();
    }

    fn right_press_selects_the_row_under_the_cursor_for_the_menu() {
        // The dangerous bug this guards: a right-click must target the row it
        // lands on, not whatever was selected before. The FileView emits a
        // `ContextSelect(Some(row))` the app drains before opening the menu.
        let mut laid = Laid::new(&items(10), (900, 600), |view| view);
        let row = laid.row_center(3);
        laid.move_to(row);
        laid.press(MouseButton::Right);
        let cmds = laid.commands();
        assert!(
            cmds.iter()
                .any(|c| matches!(c, FileCommand::ContextSelect(Some(3)))),
            "right-press emits ContextSelect for the row it lands on: {cmds:?}",
        );
    }

    fn right_press_on_empty_space_clears_the_selection_for_the_menu() {
        let mut laid = Laid::new(&items(3), (900, 600), |view| view);
        let below = laid.view.item_rect(2).unwrap().y1 + 120.0;
        let x = laid.view.list.rect().x0 + 50.0;
        laid.move_to(Point::new(x, below));
        laid.press(MouseButton::Right);
        let cmds = laid.commands();
        assert!(
            cmds.iter()
                .any(|c| matches!(c, FileCommand::ContextSelect(None))),
            "right-press on empty space clears the selection: {cmds:?}",
        );
    }

    fn sync_badge_maps_each_state_to_a_distinct_emblem() {
        let theme = Theme::dark();
        assert_eq!(sync_badge(SyncState::Local, &theme).0, Icon::Check);
        assert_eq!(
            sync_badge(SyncState::Projected, &theme).0,
            Icon::CloudDownload
        );
        let (dl_icon, dl_color) = sync_badge(SyncState::Downloading, &theme);
        assert_eq!(dl_icon, Icon::Download);
        assert_eq!(
            dl_color, theme.tokens.colors.semantic.info,
            "downloading badge shows in-flight state",
        );
    }

    fn availability_with(
        phase: &str,
        residency: &serde_json::Value,
        policy: &str,
        replicas: &serde_json::Value,
        error: bool,
    ) -> AvailabilityProjectionV1 {
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
        AvailabilityProjectionV1::parse_xattr(&serde_json::to_vec(&value).unwrap()).unwrap()
    }

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

        // A file whose bytes no device delivered is unavailable while that
        // device is offline; any other failure stays a failed transfer.
        let mut unavailable = failed.clone();
        unavailable.transfer.last_error = Some(fs_protocol::NO_PEER_DELIVERED_ERROR.into());
        let described = |availability: AvailabilityProjectionV1| {
            let mut entry = ent("report.pdf", false);
            entry.availability = Some(availability);
            accessible_entry_description(&entry)
        };
        let offline = described(unavailable);
        assert!(
            offline.contains("Unavailable until a device that keeps it is online"),
            "{offline}"
        );
        assert!(!offline.contains("Transfer failed"), "{offline}");
        let broken = described(failed);
        assert!(broken.contains("Transfer failed"), "{broken}");
        assert!(!broken.contains("Unavailable"), "{broken}");
    }

    fn dragging_a_directory_row_begins_a_pin_drag() {
        let mut entries = items(5);
        entries[2] = ent("folder", true);
        let mut laid = Laid::new(&entries, (900, 600), |view| view);
        let row = laid.row_center(2);
        laid.move_to(row);
        laid.press(MouseButton::Left);
        laid.move_to(Point::new(row.x + 70.0, row.y));
        let cmds = laid.commands();
        assert!(
            cmds.iter().any(|c| matches!(c, FileCommand::BeginDrag { path, is_dir: true } if path.ends_with("folder"))),
            "a folder press that travels begins a pin drag: {cmds:?}",
        );
    }

    fn a_file_row_press_begins_a_move_drag() {
        let mut laid = Laid::new(&items(5), (900, 600), |view| view);
        let row = laid.row_center(0);
        laid.move_to(row);
        laid.press(MouseButton::Left);
        laid.move_to(Point::new(row.x + 70.0, row.y));
        let cmds = laid.commands();
        assert!(
            cmds.iter().any(|c| matches!(c, FileCommand::BeginDrag { path, is_dir: false } if path.ends_with("f0"))),
            "a file row that travels begins a drag: {cmds:?}",
        );
    }

    #[test]
    fn apply_layout_publishes_folder_rows_as_drop_targets() {
        let mut entries = items(5); // f0..f4 are files
        entries[1] = ent("docs", true);
        entries[3] = ent("music", true);
        let targets: DropTargets = Rc::new(RefCell::new(Vec::new()));
        let laid = Laid::new(&entries, (900, 600), |view| {
            view.with_drop_targets(targets.clone())
        });
        let published = targets.borrow();
        let names: Vec<String> = published
            .iter()
            .map(|t| t.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["docs", "music"], "only folders, in row order");
        assert_eq!(published[0].rect, laid.view.item_rect(1).unwrap());
        assert!(published[0].rect.y0 < published[1].rect.y0);
    }

    #[test]
    fn list_item_at_maps_rows_below_the_header() {
        let laid = Laid::new(&items(4), (900, 600), |view| view);
        let table = laid.view.list.rect();
        let header = laid.view.list.header_height();
        assert_eq!(
            laid.view
                .item_at(Point::new(table.x0 + 50.0, table.y0 + header - 2.0)),
            None
        );
        let rows = laid.view.list.row_rects();
        assert_eq!(rows.len(), 4);
        for (index, rect) in rows {
            assert_eq!(laid.view.item_at(rect.center()), Some(index));
        }
        let below = laid.view.item_rect(3).unwrap().y1 + 40.0;
        assert_eq!(laid.view.item_at(Point::new(table.x0 + 50.0, below)), None);
    }

    fn grid_labels_share_one_baseline_and_fit_two_lines_inside_the_cell() {
        let sink = new_sink();
        let sel = SelectionModel::new();
        for text_scale in [1.0_f32, 2.0] {
            let theme = Theme::dark().with_text_scale(text_scale);
            let mut v = FileView::new(&items(6), &sel, &sink)
                .mode(ViewMode::Grid)
                .with_theme(&theme);
            v.rect = Rect::new(0.0, 0.0, 900.0, 600.0);
            v.columns = v.columns_for_width(v.rect.width());
            let cell = v.cell_rect(0);
            let art = v.grid_art_px(cell.width());
            let pad = v.geo.cell_pad * v.scale;
            let label_top = cell.y0 + pad + art + v.geo.label_gap * v.scale;
            assert!(
                v.geo.grid_icon * v.scale <= art + 1e-9,
                "the type glyph fits the art square"
            );
            assert!(
                label_top + 2.0 * v.geo.label_line + pad <= cell.y1 + 1e-9,
                "at {text_scale}x two label lines ({label_top}+{}) stay inside the cell ({})",
                2.0 * v.geo.label_line,
                cell.y1
            );
        }
    }

    fn grid_names_wrap_instead_of_being_cut() {
        let mut tr = TextRenderer::new();
        let style = Theme::dark().text(TextRole::Body);
        let cell = Geo::of(&Theme::dark()).cell_w;
        for name in [
            "Holiday photos.zip",
            "Household budget.xlsx",
            "Quarterly report.pdf",
            "Wedding invitation.pdf",
        ] {
            let lines = tr.clamp_to_lines(name, &style, (cell - 16.0) as f32, GRID_LABEL_LINES);
            assert!(lines.len() <= GRID_LABEL_LINES);
            assert_eq!(
                lines.join(" ").replace("  ", " "),
                name,
                "{name} is shown whole"
            );
        }
    }

    fn neighbouring_faces_keep_a_token_gap() {
        let sink = new_sink();
        let s1 = f64::from(Theme::dark().tokens.spacing.s1);
        let mut grid = FileView::new(&items(6), &SelectionModel::new(), &sink).mode(ViewMode::Grid);
        grid.rect = Rect::new(0.0, 0.0, 800.0, 600.0);
        grid.columns = grid.columns_for_width(800.0);
        assert!(grid.columns >= 2);
        let a = grid.cell_rect(0);
        let b = grid.cell_rect(1);
        let below = grid.cell_rect(grid.columns);
        assert!(b.x0 - a.x1 >= s1, "grid columns touch: {}", b.x0 - a.x1);
        assert!(
            below.y0 - a.y1 >= s1,
            "grid rows touch: {}",
            below.y0 - a.y1
        );
    }

    fn grid_zoom_grows_the_cell() {
        let sink = new_sink();
        let sel = SelectionModel::new();
        let mut base = FileView::new(&items(3), &sel, &sink).mode(ViewMode::Grid);
        let mut zoomed = FileView::new(&items(3), &sel, &sink)
            .mode(ViewMode::Grid)
            .with_scale(1.6);
        base.rect = Rect::new(0.0, 0.0, 1000.0, 600.0);
        zoomed.rect = base.rect;
        assert!(
            zoomed.cell_w() > base.cell_w(),
            "zoom grows grid cell width"
        );
        assert!(zoomed.cell_h() > base.cell_h());
        assert!(zoomed.row_h() > base.row_h(), "zoom grows list row height");
    }

    fn accessible_entries() -> Vec<Entry> {
        let mut entries = vec![ent("folder", true), ent("report.pdf", false)];
        entries[1].size = 4_000;
        entries[1].availability = Some(availability_with(
            "idle",
            &serde_json::json!({"state": "resident", "bytes": 4000}),
            "pinned",
            &serde_json::json!({"state": "known", "reachable": 2, "durable": 1}),
            false,
        ));
        entries[1].history = Some(conflicted_history());

        entries
    }

    fn conflicted_history() -> fs_protocol::HistoryProjectionV1 {
        let current = "a0d4e23f-c7cb-4ded-a427-b32cf9da9344";
        let other = "a0d4e23f-c7cb-4ded-a427-b32cf9da9342";
        fs_protocol::HistoryProjectionV1::parse_xattr(
            &serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                "total_versions": 2,
                "versions_truncated": false,
                "heads": [current, other],
                "versions": [{
                    "id": other,
                    "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                    "parents": [],
                    "actor": {"person": vec![1; 32], "device": vec![2; 32]},
                    "timestamp_ms": 9,
                    "content_hash": vec![3; 32],
                    "metadata_hash": vec![4; 32],
                    "provenance": "Edit"
                }, {
                    "id": current,
                    "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                    "parents": [other],
                    "actor": {"person": vec![6; 32], "device": vec![7; 32]},
                    "timestamp_ms": 10,
                    "content_hash": vec![5; 32],
                    "metadata_hash": vec![8; 32],
                    "provenance": "Edit"
                }],
                "conflicts_truncated": false,
                "conflicts": [{
                    "id": "c0d4e23f-c7cb-4ded-a427-b32cf9da9343",
                    "record_id": "b0d4e23f-c7cb-4ded-a427-b32cf9da9341",
                    "versions": [other, current],
                    "status": "Open",
                    "resolution": null
                }]
            }))
            .unwrap(),
        )
        .expect("the projected history is well formed")
    }

    fn assert_description_has_every_fact(description: Option<&str>) {
        let description = description.unwrap_or_default();
        for fact in [
            "File, 4 kB",
            "Available offline",
            "Kept offline",
            "Kept on other devices",
            "Has a version conflict",
        ] {
            assert!(
                description.contains(fact),
                "missing `{fact}` in {description:?}"
            );
        }
    }

    struct Laid {
        view: FileView,
        frame: ui::launch::ShotFrame,
        sink: CommandSink,
    }

    impl Laid {
        fn new(
            entries: &[Entry],
            size: (u32, u32),
            build: impl FnOnce(FileView) -> FileView,
        ) -> Self {
            Self::with_theme(entries, size, Theme::dark(), build)
        }

        fn with_theme(
            entries: &[Entry],
            size: (u32, u32),
            theme: Theme,
            build: impl FnOnce(FileView) -> FileView,
        ) -> Self {
            let sink = new_sink();
            let mut view = build(FileView::new(entries, &SelectionModel::new(), &sink));
            let mut frame = ui::launch::ShotFrame::new(size, theme);
            frame.layout(&mut view).unwrap();
            let _ = frame.draw(&mut view);
            Self { view, frame, sink }
        }

        fn relayout(&mut self) {
            self.frame.layout(&mut self.view).unwrap();
            let _ = self.frame.draw(&mut self.view);
        }

        fn send(&mut self, event: &WindowEvent) {
            let (_, tree, node) = self.frame.layout_parts().expect("laid out");
            let tree = tree.clone();
            ui::testing::Dispatch::new()
                .layout(&tree, node)
                .send(&mut self.view, event);
        }

        fn move_to(&mut self, at: Point) {
            self.send(&WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: (at.x, at.y).into(),
            });
        }

        fn press(&mut self, button: MouseButton) {
            self.send(&ui::synthetic_input::mouse_input(
                ElementState::Pressed,
                button,
            ));
        }

        fn row_center(&self, index: usize) -> Point {
            self.view
                .item_rect(index)
                .expect("the row is on screen")
                .center()
        }

        fn commands(&self) -> Vec<FileCommand> {
            self.sink.borrow().iter().cloned().collect()
        }
    }

    #[test]
    fn the_list_is_a_table_with_a_sorted_column_and_a_row_per_entry() {
        let laid = Laid::new(&items(4), (900, 600), |view| view);
        let node = Widget::accessibility(&laid.view).expect("list semantics");
        assert_eq!(node.role, ui::a11y::Role::Table);
        assert_eq!(node.name.as_deref(), Some("Files"));
        assert_eq!(
            node.description.as_deref(),
            Some("sorted by Name ascending")
        );
        assert_eq!(node.children.len(), 4);
        let headers: Vec<_> = node.children[0]
            .children
            .iter()
            .filter_map(|cell| cell.name.clone())
            .filter(|name| !name.is_empty())
            .collect();
        assert_eq!(headers, ["Name", "Size", "Modified"]);
        assert_eq!(node.children[2].name.as_deref(), Some("f2"));
        assert_eq!(node.children[2].pos_in_set, Some(3));
    }

    #[test]
    fn list_rows_follow_item_zoom_and_the_header_band_is_the_tables() {
        let base = Laid::new(&items(3), (900, 600), |view| view);
        let zoomed = Laid::new(&items(3), (900, 600), |view| view.with_scale(1.6));
        let height = |laid: &Laid| laid.view.item_rect(0).unwrap().height();
        assert!(
            height(&zoomed) > height(&base),
            "zoom grows list row height"
        );
        let table = base.view.list.rect();
        let first = base.view.item_rect(0).unwrap();
        assert!(
            (first.y0 - (table.y0 + base.view.list.header_height())).abs() < 0.01,
            "the first row starts where the header band the view reserves ends"
        );
        let scaled = Theme::dark().with_text_scale(2.0);
        let large = Laid::with_theme(&items(3), (900, 600), scaled.clone(), |view| {
            view.with_theme(&scaled)
        });
        assert!(
            large.view.item_rect(0).unwrap().height()
                >= f64::from(scaled.text(TextRole::Body).line_height_px()),
            "large type needs room in its rows"
        );
        let table = large.view.list.rect();
        assert!(
            (large.view.item_rect(0).unwrap().y0 - (table.y0 + large.view.list.header_height()))
                .abs()
                < 0.01,
            "the header band follows the type"
        );
    }

    #[test]
    fn a_large_directory_mounts_only_the_visible_rows() {
        let mut laid = Laid::new(&items(100_000), (900, 600), |view| view);
        let first = laid.view.list.row_rects();
        assert!(first.len() < 40, "{} rows mounted", first.len());
        assert_eq!(first[0].0, 0);
        laid.view.list.set_scroll_y(34_000.0);
        laid.relayout();
        let later = laid.view.list.row_rects();
        assert!(later.len() < 40);
        assert!(later[0].0 > 900, "the window moved to row {}", later[0].0);
    }

    #[test]
    fn the_list_scrolls_by_wheel_and_keeps_its_place_across_rebuilds() {
        let mut laid = Laid::new(&items(300), (900, 600), |view| view);
        let slot = laid.view.list.clone();
        let centre = laid.view.list.rect().center();
        laid.move_to(centre);
        laid.send(&ui::synthetic_input::mouse_wheel(0.0, -5.0));
        let scrolled = slot.scroll_y();
        assert!(scrolled > 0.0, "a wheel-down scrolls the list");
        assert!(slot.max_scroll_y() >= scrolled);

        let sink = new_sink();
        let mut rebuilt =
            FileView::new(&items(300), &SelectionModel::new(), &sink).with_list(slot.clone());
        let mut frame = ui::launch::ShotFrame::new((900, 600), Theme::dark());
        frame.layout(&mut rebuilt).unwrap();
        assert_eq!(
            slot.scroll_y(),
            scrolled,
            "the scroll position survives a rebuild"
        );

        slot.reset_scroll();
        assert_eq!(slot.scroll_y(), 0.0);
    }

    #[test]
    fn narrow_panes_drop_detail_columns_and_ask_for_a_rebuild() {
        let mut wide = Laid::new(&items(4), (1000, 600), |view| view);
        assert!(
            !Widget::take_structure_dirty(&mut wide.view),
            "wide keeps every column"
        );

        let mut laid = Laid::new(&items(4), (900, 600), |view| view);
        assert!(!Widget::take_structure_dirty(&mut laid.view));
        let mut medium = Laid::new(&items(4), (560, 600), |view| view);
        assert!(
            Widget::take_structure_dirty(&mut medium.view),
            "a pane narrower than the columns it was built with asks for a rebuild"
        );

        let slot = medium.view.list.clone();
        let sink = new_sink();
        let mut rebuilt = FileView::new(&items(4), &SelectionModel::new(), &sink).with_list(slot);
        let mut frame = ui::launch::ShotFrame::new((560, 600), Theme::dark());
        frame.layout(&mut rebuilt).unwrap();
        assert!(!Widget::take_structure_dirty(&mut rebuilt));
        let node = Widget::accessibility(&rebuilt).unwrap();
        let headers: Vec<_> = node.children[0]
            .children
            .iter()
            .filter_map(|cell| cell.name.clone())
            .filter(|name| !name.is_empty())
            .collect();
        assert_eq!(
            headers,
            ["Name", "Size", "Modified"],
            "the date column stays at 560"
        );

        let geo = Geo::of(&Theme::dark());
        assert_eq!(shape_columns(1000.0, &geo, 1.0, 1.0), (true, 2));
        assert_eq!(shape_columns(500.0, &geo, 1.0, 1.0), (true, 1));
        assert_eq!(shape_columns(250.0, &geo, 1.0, 1.0), (false, 0));
        assert_eq!(shape_columns(360.0, &geo, 1.6, 1.0), (false, 0));
        assert_eq!(shape_columns(900.0, &geo, 1.0, 2.0), (true, 1));
    }

    #[test]
    fn header_clicks_sort_by_the_column() {
        let mut laid = Laid::new(&items(4), (900, 600), |view| view);
        let node = Widget::accessibility(&laid.view).unwrap();
        let cell = |column: usize| node.children[0].children[column].bounds.unwrap().to_rect();
        let header_y = laid.view.list.rect().y0 + laid.view.list.header_height() / 2.0;
        for (column, command) in [(0, 1), (1, 2), (2, 3)] {
            laid.sink.borrow_mut().clear();
            let x = cell(column).center().x;
            laid.move_to(Point::new(x, header_y));
            laid.press(MouseButton::Left);
            assert!(
                laid.commands()
                    .iter()
                    .any(|c| matches!(c, FileCommand::SortColumn(i) if *i == command)),
                "column {column} sorts as column {command}: {:?}",
                laid.commands()
            );
        }
        laid.sink.borrow_mut().clear();
        let row = laid.row_center(1);
        laid.move_to(row);
        laid.press(MouseButton::Left);
        assert!(
            !laid
                .commands()
                .iter()
                .any(|c| matches!(c, FileCommand::SortColumn(_)))
        );
    }

    #[test]
    fn a_press_on_a_list_row_selects_it_and_arms_a_drag() {
        let mut laid = Laid::new(&items(5), (900, 600), |view| view);
        let row = laid.row_center(2);
        laid.move_to(row);
        laid.press(MouseButton::Left);
        let commands = laid.commands();
        assert!(
            commands.iter().any(|c| matches!(
                c,
                FileCommand::Click {
                    index: 2,
                    ctrl: false,
                    shift: false
                }
            )),
            "{commands:?}"
        );
        assert!(
            commands
                .iter()
                .any(|c| matches!(c, FileCommand::ArmDrag { path, .. } if path.ends_with("f2"))),
            "{commands:?}"
        );
        assert!(
            Widget::accessibility(&laid.view).unwrap().children[2]
                .state
                .selected,
            "the table shows the row selected at once"
        );
    }

    #[test]
    fn list_rows_and_drop_targets_stay_above_the_status_lane() {
        let mut entries = items(60);
        for entry in &mut entries {
            entry.is_dir = true;
        }
        let targets: DropTargets = Rc::new(RefCell::new(Vec::new()));
        let laid = Laid::new(&entries, (900, 600), |view| {
            view.with_bottom_inset(64.0)
                .with_drop_targets(targets.clone())
        });
        let table = laid.view.list.rect();
        assert!(
            table.y1 <= 600.0 - 64.0 + 0.5,
            "the table ends above the status lane: {table:?}"
        );
        let published = targets.borrow();
        assert!(
            !published.is_empty() && published.len() < 32,
            "{}",
            published.len()
        );
        assert!(
            published
                .iter()
                .all(|target| target.rect.y1 <= table.y1 + 0.5 && target.rect.y0 >= table.y0),
            "no drop target lies in the status lane"
        );
    }

    #[test]
    fn the_list_exposes_each_file_selection_and_collaboration_state() {
        let mut selection = SelectionModel::new();
        selection.select_only(1);
        let entries = accessible_entries();
        let sink = new_sink();
        let mut view = FileView::new(&entries, &selection, &sink);
        let mut frame = ui::launch::ShotFrame::new((900, 600), Theme::dark());
        frame.layout(&mut view).unwrap();
        let node = Widget::accessibility(&view).expect("file view has semantics");
        assert_eq!(node.name.as_deref(), Some("Files"));
        assert_eq!(node.children.len(), 2);
        assert_eq!(node.children[0].name.as_deref(), Some("folder"));
        assert!(!node.children[0].state.selected);
        assert_eq!(node.children[1].name.as_deref(), Some("report.pdf"));
        assert!(node.children[1].state.selected);
        assert_eq!(node.children[1].pos_in_set, Some(2));
        assert_eq!(node.children[1].set_size, Some(2));
        assert_description_has_every_fact(node.children[1].description.as_deref());
    }
}
