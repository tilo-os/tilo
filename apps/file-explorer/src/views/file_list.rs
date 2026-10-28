//! The file list: the toolkit's shared file list
//! ([`ui::widget::file_table::FileTable`]) hosting a folder's entries.
//!
//! The shared list is the one every list of files on the desktop is: its
//! columns, their breakpoints, the header, the rows, the selection lift, the
//! sort indicator, the scrollbar, keyboard and touch. Files adds only what is
//! its own:
//!
//! * the name cell, which draws the file's icon or preview, its sync emblem
//!   and its name, and leaves room for the in-place rename;
//! * the Size column's folder counts;
//! * the press on a row, which the file view keeps (it selects and arms the
//!   drag that carries the row).
//!
//! The list lives in a [`ListSlot`] the app owns, so its scroll position,
//! its column widths and its scroll anchor survive the app's per-change
//! rebuild of the file view. Everything a rebuild changes (entries,
//! selection, sort, zoom, the rename in progress) goes in through
//! `ListSlot::sync`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use ui::SelectionModel;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::file_table::{FileColumn, FileRow, FileTable, FileTableInputs, FileTableMetrics};

use super::file_view::{
    Geo, ThumbCtx, VisualPaint, paint_entry_emblem, paint_file_visual, truncate,
};
use crate::i18n::t;
use crate::model::{Entry, SortBy, SortOrder, human_size};
use ui::icons::{Icon, LUCIDE_DEFAULT_STROKE_PX, draw_icon};

/// What every row of one build shares: the metrics and services its name cell
/// paints with. Rows hold it by `Rc`, so the table's column closures need
/// capture nothing.
pub(super) struct RowContext {
    scale: f64,
    geo: Geo,
    thumbs: Option<ThumbCtx>,
    folder_counts: Option<std::sync::Arc<crate::folder_counts::FolderCounts>>,
    show_hidden: bool,
    workspace_roots: Rc<[std::path::PathBuf]>,
    rename_rect: Option<Rc<Cell<Option<Rect>>>>,
}

/// One row: an entry and the context its cells paint with.
pub(super) struct ListRow {
    entry: Entry,
    renaming: bool,
    cx: Rc<RowContext>,
}

impl FileRow for ListRow {
    fn path(&self) -> &std::path::Path {
        &self.entry.path
    }

    fn name(&self) -> &str {
        &self.entry.name
    }

    fn is_dir(&self) -> bool {
        self.entry.is_dir
    }

    /// A file's size, or how many items a folder holds once counted (blank
    /// until then).
    fn size_text(&self) -> String {
        if !self.entry.is_dir {
            return human_size(&self.entry);
        }
        self.cx
            .folder_counts
            .as_ref()
            .and_then(|counts| {
                counts.count(&self.entry.path, self.entry.modified, self.cx.show_hidden)
            })
            .map(ui::format::items)
            .unwrap_or_default()
    }

    fn modified(&self) -> Option<chrono::DateTime<chrono::Local>> {
        self.entry.modified
    }

    /// An app's kind in the Applications place.
    fn kind(&self) -> Option<String> {
        self.entry
            .app
            .as_ref()
            .map(|app| crate::applications::kind_label(app.kind))
    }

    /// The file's icon or preview with its sync emblem, then its name and,
    /// for a conflicted file, a warning.
    fn paint_name(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        tr: &mut TextRenderer,
        cell: Rect,
        _metrics: &FileTableMetrics,
    ) {
        paint_name_cell(self, scene, transform, theme, tr, cell);
    }
}

/// The shared list's measures, from the file view's own geometry.
fn metrics_of(geo: &Geo, scale: f64, text_scale: f64) -> FileTableMetrics {
    FileTableMetrics {
        scale,
        text_scale,
        row: geo.row,
        icon: geo.icon,
        icon_label_gap: geo.icon_label_gap,
        icon_lead: geo.icon_lead,
        size_col: geo.size_col,
        modified_col: geo.modified_col,
        date_col: geo.date_col,
        unit: geo.cell_w,
        scrollbar_gutter: geo.scrollbar_gutter,
    }
}

/// The columns a pane `width` wide shows, as `(size, modified)`: whether the
/// size is shown, and how much of the modified time (0 none, 1 the date, 2 the
/// date and time of day).
#[cfg(test)]
pub(super) fn shape_columns(width: f64, geo: &Geo, scale: f64, text_scale: f64) -> (bool, u8) {
    use ui::widget::file_table::{Modified, Shape};
    let shape = Shape::for_width(width, &metrics_of(geo, scale, text_scale));
    let modified = match shape.modified {
        Modified::Hidden => 0,
        Modified::Date => 1,
        Modified::DateTime => 2,
    };
    (shape.size, modified)
}

/// What the file view reads after forwarding an event to the table.
pub(super) struct ListOutcome {
    /// A press landed on the header of the column with this sort key.
    pub(super) sort_key: Option<&'static str>,
}

/// The list the app keeps across rebuilds of the file view.
#[derive(Clone)]
pub struct ListSlot(Rc<RefCell<FileTable<ListRow>>>);

impl ListSlot {
    /// The column widths the person set by dragging an edge.
    #[must_use]
    pub fn widths(&self) -> std::collections::BTreeMap<String, u32> {
        self.0
            .try_borrow()
            .map(|table| {
                table
                    .column_widths()
                    .into_iter()
                    .map(|(key, px)| {
                        #[allow(
                            clippy::cast_possible_truncation,
                            clippy::cast_sign_loss,
                            reason = "a column is a few hundred whole pixels across"
                        )]
                        let whole = px.max(0.0) as u32;
                        (key, whole)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Lay the columns out at the widths the person set on another launch.
    pub fn set_widths(&self, widths: &std::collections::BTreeMap<String, u32>) {
        if widths.is_empty() {
            return;
        }
        if let Ok(mut table) = self.0.try_borrow_mut() {
            table.set_column_widths(widths.iter().map(|(key, px)| (key.clone(), *px as f32)));
        }
    }
}

impl Default for ListSlot {
    fn default() -> Self {
        Self(Rc::new(RefCell::new(FileTable::new(
            "file_list",
            t("files-a11y-list"),
        ))))
    }
}

/// What a rebuild puts into the table.
pub(super) struct ListInputs<'a> {
    pub entries: &'a [Entry],
    pub selection: &'a SelectionModel,
    pub sort_by: SortBy,
    pub sort_order: SortOrder,
    pub scale: f64,
    pub text_scale: f64,
    pub geo: Geo,
    pub thumbs: Option<ThumbCtx>,
    pub folder_counts: Option<std::sync::Arc<crate::folder_counts::FolderCounts>>,
    pub show_hidden: bool,
    pub workspace_roots: Rc<[std::path::PathBuf]>,
    pub renaming: Option<usize>,
    pub rename_rect: Option<Rc<Cell<Option<Rect>>>>,
    pub theme: &'a Theme,
}

/// The column a `SortBy` orders by.
fn column_of(by: SortBy) -> FileColumn {
    match by {
        SortBy::Size => FileColumn::Size,
        SortBy::Modified => FileColumn::Modified,
        SortBy::Name | SortBy::Type => FileColumn::Name,
    }
}

/// The `SortColumn` command index of a column key (1 name, 2 size, 3 date).
pub(super) fn sort_column(key: &str) -> Option<usize> {
    match key {
        "name" => Some(1),
        "size" => Some(2),
        "modified" => Some(3),
        _ => None,
    }
}

/// The name cell: the file's icon or preview with its sync emblem, then its
/// name and, for a conflicted file, a warning. (The shared list's own text
/// cells paint the other columns.)
fn paint_name_cell(
    row: &ListRow,
    scene: &mut Scene,
    transform: Affine,
    theme: &Theme,
    tr: &mut TextRenderer,
    cell: Rect,
) {
    let cx = &*row.cx;
    let geo = &cx.geo;
    let tokens = &theme.tokens;
    let pad = f64::from(tokens.spacing.s2);
    let icon_px = geo.icon * cx.scale;
    let lead_x = cell.x0 + pad;
    let lead_y = cell.y0 + (cell.height() - icon_px) / 2.0;
    let icon_rect = Rect::new(lead_x, lead_y, lead_x + icon_px, lead_y + icon_px);
    let ink = if row.entry.is_dir {
        tokens.colors.text_primary
    } else {
        tokens.colors.text_dim
    };
    let visual = VisualPaint {
        thumbs: cx.thumbs.as_ref(),
        workspace_roots: &cx.workspace_roots,
        scale: cx.scale,
        glyph_px: icon_px,
        glyph_color: ink,
    };
    paint_file_visual(scene, transform, tr, theme, &visual, icon_rect, &row.entry);
    paint_entry_emblem(scene, transform, icon_rect, &row.entry, cx.scale, theme);

    let name_x = lead_x + icon_px + geo.icon_label_gap;
    if row.renaming {
        // The app draws the in-place rename editor where the name was.
        if let Some(sink) = &cx.rename_rect {
            let inset = f64::from(tokens.spacing.s1) / 2.0;
            sink.set(Some(Rect::new(
                name_x - f64::from(theme.padding_standard),
                cell.y0 + inset,
                cell.x1 - geo.icon_lead,
                cell.y1 - inset,
            )));
        }
        return;
    }
    let body = theme.text(TextRole::Body);
    let avail = (cell.x1 - pad - name_x).max(0.0);
    let label = truncate(tr, &row.entry.name, &body, avail);
    let (label_w, line_h) = tr.measure_style(&label, None, &body);
    let text_y = cell.y0 + (cell.height() - f64::from(line_h)) / 2.0;
    tr.draw_style(
        scene,
        &label,
        tokens.colors.text_primary,
        transform * Affine::translate((name_x, text_y)),
        None,
        &body,
    );
    if row.entry.is_conflict() {
        let side = f64::from(tokens.metrics.icon_sm);
        let x = name_x + f64::from(label_w) + f64::from(tokens.spacing.s2);
        if x + side <= cell.x1 - pad {
            let y = cell.y0 + (cell.height() - side) / 2.0;
            draw_icon(
                scene,
                transform,
                Rect::new(x, y, x + side, y + side),
                Icon::TriangleAlert,
                tokens.colors.semantic.warning,
                LUCIDE_DEFAULT_STROKE_PX,
            );
        }
    }
}

impl ListSlot {
    /// Put this build's entries, selection and sort into the table, making
    /// it first if there is none or the columns changed.
    pub(super) fn sync(&self, inputs: &ListInputs<'_>) {
        let cx = Rc::new(RowContext {
            scale: inputs.scale,
            geo: inputs.geo,
            thumbs: inputs.thumbs.clone(),
            folder_counts: inputs.folder_counts.clone(),
            show_hidden: inputs.show_hidden,
            workspace_roots: Rc::clone(&inputs.workspace_roots),
            rename_rect: inputs.rename_rect.clone(),
        });
        let rows: Vec<ListRow> = inputs
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| ListRow {
                entry: entry.clone(),
                renaming: inputs.renaming == Some(index),
                cx: Rc::clone(&cx),
            })
            .collect();
        // The app hands over entries already in the order it sorted them; the
        // list shows which column that is by.
        self.0.borrow_mut().sync(FileTableInputs {
            rows,
            selection: inputs.selection,
            sort: column_of(inputs.sort_by),
            ascending: inputs.sort_order == SortOrder::Ascending,
            metrics: metrics_of(&inputs.geo, inputs.scale, inputs.text_scale),
            theme: inputs.theme,
        });
    }

    /// Scroll back to the top (a folder was opened).
    pub fn reset_scroll(&self) {
        self.0.borrow_mut().reset_scroll();
    }

    /// The table's scroll offset.
    #[must_use]
    pub fn scroll_y(&self) -> f32 {
        self.0.borrow().scroll_y()
    }

    /// Scroll to `y` (clamped by the table).
    pub fn set_scroll_y(&self, y: f32) {
        self.0.borrow_mut().set_scroll_y(y);
    }

    /// The farthest the list can scroll.
    #[must_use]
    pub fn max_scroll_y(&self) -> f32 {
        self.0.borrow().max_scroll_y()
    }

    /// The table's laid-out rectangle.
    #[must_use]
    pub fn rect(&self) -> Rect {
        self.0.borrow().rect()
    }

    /// Add the table's node under `parent`'s layout and size it to fill.
    pub(super) fn build_layout(&self, taffy: &mut TaffyTree<()>, theme: &Theme) -> Option<NodeId> {
        self.0.borrow_mut().build_layout(taffy, theme)
    }

    /// Lay the table out in `node`. Returns whether the columns should change
    /// with the new width, so the view is rebuilt.
    pub(super) fn apply_layout(&self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) -> bool {
        self.0.borrow_mut().apply_layout(taffy, node, origin)
    }

    pub(super) fn draw(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.0
            .borrow_mut()
            .draw(scene, transform, tr, theme, presence);
    }

    /// Forward `event` to the table and report what its callbacks recorded.
    pub(super) fn handle_event(
        &self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> ListOutcome {
        let outcome = self.0.borrow_mut().handle_event(event, ctx);
        ListOutcome {
            sort_key: outcome.sort.map(FileColumn::key),
        }
    }

    /// Whether `at` is over the table's scrollbar, which the table keeps for
    /// itself (a press there scrolls; a press on a row is the file view's).
    pub(super) fn in_scrollbar(&self, at: Point) -> bool {
        self.0.borrow().in_scrollbar(at)
    }

    /// Show `selection` in the table (a click was just applied to the view's
    /// own copy).
    pub(super) fn select(&self, selection: &SelectionModel) {
        self.0.borrow_mut().select(selection);
    }

    /// Tell the table where the pointer is, so a press that arrives with no
    /// move since the view was rebuilt still lands on the right row.
    pub(super) fn point_at(&self, at: Point, ctx: &mut ui::widget::EventCtx<'_>) {
        self.0.borrow_mut().point_at(at, ctx);
    }

    pub(super) fn update(
        &self,
        state: &ui::widget::AppModel,
        path: &ui::widget::ModelPath,
    ) -> bool {
        self.0.borrow_mut().update(state, path)
    }

    pub(super) fn needs_animation_frame(&self) -> bool {
        self.0.borrow().needs_animation_frame()
    }

    pub(super) fn is_layout_dirty(&self) -> bool {
        self.0.borrow().is_layout_dirty()
    }

    pub(super) fn wants_pointer_capture(&self) -> bool {
        self.0.borrow().wants_pointer_capture()
    }

    /// The table swaps its retained cell controls on a scroll; Files has no
    /// such cells, so this never asks for a rebuild.
    pub(super) fn discard_structure_dirty(&self) {
        self.0.borrow_mut().discard_structure_dirty();
    }

    /// The header band's height as the table lays it out.
    pub(super) fn header_height(&self) -> f64 {
        self.0.borrow().header_height()
    }

    /// Each visible row's rectangle by display index, clipped to the body
    /// below the header band.
    pub(super) fn row_rects(&self) -> Vec<(usize, Rect)> {
        self.0.borrow().row_rects()
    }

    /// The table's accessibility node.
    pub(super) fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        self.0.borrow().accessibility()
    }
}
