//! The file list: the toolkit's shared table, with Files' name cell and the
//! widths the person dragged.

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

pub(super) struct RowContext {
    scale: f64,
    geo: Geo,
    thumbs: Option<ThumbCtx>,
    folder_counts: Option<std::sync::Arc<crate::folder_counts::FolderCounts>>,
    show_hidden: bool,
    workspace_roots: Rc<[std::path::PathBuf]>,
    rename_rect: Option<Rc<Cell<Option<Rect>>>>,
}

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

    fn kind(&self) -> Option<String> {
        self.entry
            .app
            .as_ref()
            .map(|app| crate::applications::kind_label(app.kind))
    }

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

pub(super) struct ListOutcome {
    pub(super) sort_key: Option<&'static str>,
}

#[derive(Clone)]
pub struct ListSlot(Rc<RefCell<FileTable<ListRow>>>);

impl ListSlot {
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
        Self(Rc::new(RefCell::new(
            FileTable::new("file_list", t("files-a11y-list")).handles_row_menu(),
        )))
    }
}

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

fn column_of(by: SortBy) -> FileColumn {
    match by {
        SortBy::Size => FileColumn::Size,
        SortBy::Modified => FileColumn::Modified,
        SortBy::Name | SortBy::Type => FileColumn::Name,
    }
}

pub(super) fn sort_column(key: &str) -> Option<usize> {
    match key {
        "name" => Some(1),
        "size" => Some(2),
        "modified" => Some(3),
        _ => None,
    }
}

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
        self.0.borrow_mut().sync(FileTableInputs {
            rows,
            selection: inputs.selection,
            sort: column_of(inputs.sort_by),
            ascending: inputs.sort_order == SortOrder::Ascending,
            metrics: metrics_of(&inputs.geo, inputs.scale, inputs.text_scale),
            theme: inputs.theme,
        });
    }

    pub fn reset_scroll(&self) {
        self.0.borrow_mut().reset_scroll();
    }

    #[must_use]
    pub fn scroll_y(&self) -> f32 {
        self.0.borrow().scroll_y()
    }

    pub fn set_scroll_y(&self, y: f32) {
        self.0.borrow_mut().set_scroll_y(y);
    }

    #[must_use]
    pub fn max_scroll_y(&self) -> f32 {
        self.0.borrow().max_scroll_y()
    }

    #[must_use]
    pub fn rect(&self) -> Rect {
        self.0.borrow().rect()
    }

    pub(super) fn build_layout(&self, taffy: &mut TaffyTree<()>, theme: &Theme) -> Option<NodeId> {
        self.0.borrow_mut().build_layout(taffy, theme)
    }

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

    pub(crate) fn take_row_requests(&self) -> (Option<usize>, Option<(usize, Point)>) {
        self.0.borrow_mut().take_row_requests()
    }

    pub(super) fn in_scrollbar(&self, at: Point) -> bool {
        self.0.borrow().in_scrollbar(at)
    }

    pub(super) fn select(&self, selection: &SelectionModel) {
        self.0.borrow_mut().select(selection);
    }

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

    pub(super) fn header_height(&self) -> f64 {
        self.0.borrow().header_height()
    }

    pub(super) fn row_rects(&self) -> Vec<(usize, Rect)> {
        self.0.borrow().row_rects()
    }

    pub(super) fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        self.0.borrow().accessibility()
    }

    pub(super) fn perform_semantic_action(
        &self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        path: &ui::widget::ModelPath,
    ) -> bool {
        self.0
            .borrow_mut()
            .perform_semantic_action(target, action, state, path)
    }
}
