//! The file list: the framework's `DataTable` hosting a folder's entries.
//!
//! The table paints the header, the rows, the selection lift, the sort
//! indicator, the numeric columns and the scrollbar, and owns scrolling and
//! column resizing. Files adds only what is its own:
//!
//! * the name cell, which draws the file's icon or preview, its sync emblem
//!   and its name (the table has no leading icon cell);
//! * the rows' rectangles, which it reads back from the table's accessibility
//!   export so a right-click, a drag and the folder drop targets resolve to
//!   the row the table laid out (the table publishes no row geometry).
//!
//! The table lives in a [`ListSlot`] the app owns, so its scroll position,
//! its column widths and its scroll anchor survive the app's per-change
//! rebuild of the file view. Everything a rebuild changes (entries,
//! selection, sort, zoom, the rename in progress) goes in through
//! [`ListSlot::sync`].

use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use taffy::{Dimension, NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use ui::SelectionModel;
use ui::presence::PresenceCtx;
use ui::text_render::{TextKind, TextRenderer};
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::Widget;
use ui::widget::data_table::{ColumnDef, ColumnWidth, DataTable, text_band, type_line_height};

use super::file_view::{
    Geo, ThumbCtx, VisualPaint, paint_entry_emblem, paint_file_visual, truncate,
};
use crate::i18n::t;
use crate::model::{Entry, SortBy, SortOrder, format_modified, format_modified_date, human_size};
use ui::icons::{Icon, LUCIDE_DEFAULT_STROKE_PX, draw_icon};

/// What every row of one build shares: the metrics and services its name cell
/// paints with. Rows hold it by `Rc`, so the table's column closures need
/// capture nothing.
pub(super) struct RowContext {
    scale: f64,
    geo: Geo,
    thumbs: Option<ThumbCtx>,
    rename_rect: Option<Rc<Cell<Option<Rect>>>>,
}

/// One row: an entry and the context its cells paint with.
pub(super) struct ListRow {
    entry: Entry,
    renaming: bool,
    cx: Rc<RowContext>,
}

/// How much of a file's modified time its column shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Modified {
    /// The column is dropped.
    Hidden,
    /// The date alone.
    Date,
    /// The date and the time of day.
    DateTime,
}

/// Which optional columns the pane is wide enough for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Shape {
    size: bool,
    modified: Modified,
}

impl Shape {
    /// The columns a pane `width` wide shows: the name always has room, so
    /// the detail columns shrink and then drop out on a narrow pane (and
    /// sooner with larger type or zoom).
    fn for_width(width: f64, geo: &Geo, scale: f64, text_scale: f64) -> Self {
        if width <= 0.0 {
            return Self {
                size: true,
                modified: Modified::DateTime,
            };
        }
        let unit = geo.cell_w * scale * text_scale;
        Self {
            size: width > unit * 2.2,
            modified: if width > unit * 4.6 {
                Modified::DateTime
            } else if width > unit * 3.0 {
                Modified::Date
            } else {
                Modified::Hidden
            },
        }
    }
}

/// The columns a pane `width` wide shows, as `(size, modified)`: whether the
/// size is shown, and how much of the modified time (0 none, 1 the date, 2 the
/// date and time of day).
#[cfg(test)]
pub(super) fn shape_columns(width: f64, geo: &Geo, scale: f64, text_scale: f64) -> (bool, u8) {
    let shape = Shape::for_width(width, geo, scale, text_scale);
    let modified = match shape.modified {
        Modified::Hidden => 0,
        Modified::Date => 1,
        Modified::DateTime => 2,
    };
    (shape.size, modified)
}

/// What the table's callbacks hand back to the file view after an event.
#[derive(Default)]
struct Mailbox {
    header_click: Cell<Option<usize>>,
}

/// What the file view reads after forwarding an event to the table.
pub(super) struct ListOutcome {
    /// A press landed on the header of the column with this sort key.
    pub(super) sort_key: Option<&'static str>,
}

#[derive(Default)]
struct ListState {
    table: Option<DataTable<ListRow>>,
    shape: Option<Shape>,
    mailbox: Rc<Mailbox>,
    /// The table's width at its last layout, which decides its columns.
    width: f64,
    /// The body height and row height last given to the table.
    viewport: (f32, f32),
    /// The header band, from the theme's type (the table sizes it the same way).
    header_h: f64,
    /// What the columns are fitted with, kept for the layout that re-fits them.
    fit: Option<(Geo, f64, f64)>,
    /// The scroll position to restore on the next layout after a rebuild.
    reset_scroll: bool,
}

/// The list the app keeps across rebuilds of the file view.
#[derive(Clone, Default)]
pub struct ListSlot(Rc<RefCell<ListState>>);

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
    pub renaming: Option<usize>,
    pub rename_rect: Option<Rc<Cell<Option<Rect>>>>,
    pub theme: &'a Theme,
}

/// The sort key of the column a `SortBy` orders by.
fn key_of(by: SortBy) -> &'static str {
    match by {
        SortBy::Size => "size",
        SortBy::Modified => "modified",
        SortBy::Name | SortBy::Type => "name",
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

/// A stable identity for a row: its path.
fn row_key(row: &ListRow) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    row.entry.path.hash(&mut hasher);
    hasher.finish()
}

fn build_table(
    shape: Shape,
    mailbox: &Rc<Mailbox>,
    geo: &Geo,
    text_scale: f64,
) -> DataTable<ListRow> {
    let mut columns = vec![
        ColumnDef::new("name", t("files-column-name"), ColumnWidth::Flex(1.0))
            .sortable(true)
            .with_render(Box::new(paint_name_cell))
            .with_accessible_value(|row: &ListRow| row.entry.name.clone()),
    ];
    if shape.size {
        columns.push(
            ColumnDef::new(
                "size",
                t("files-column-size"),
                ColumnWidth::Fixed((geo.size_col * text_scale) as f32),
            )
            .sortable(true)
            .with_kind(TextKind::Numeric)
            .with_optional_text(
                |row: &ListRow| (!row.entry.is_dir).then(|| human_size(&row.entry)),
                "—",
            ),
        );
    }
    if shape.modified != Modified::Hidden {
        let with_time = shape.modified == Modified::DateTime;
        let width = if with_time {
            geo.modified_col
        } else {
            geo.date_col
        };
        columns.push(
            ColumnDef::new(
                "modified",
                t("files-column-modified"),
                ColumnWidth::Fixed((width * text_scale) as f32),
            )
            .sortable(true)
            .with_kind(TextKind::Numeric)
            .with_optional_text(
                move |row: &ListRow| {
                    if with_time {
                        row.entry.modified.map(|at| format_modified(Some(at)))
                    } else {
                        format_modified_date(row.entry.modified)
                    }
                },
                "—",
            ),
        );
    }
    // The table's scrollbar overlays its last column's trailing edge; an empty
    // column of the scrollbar's gutter keeps the last column's figures clear
    // of it.
    columns.push(ColumnDef::new(
        "end",
        "",
        ColumnWidth::Fixed(geo.scrollbar_gutter as f32),
    ));
    let header = Rc::clone(mailbox);
    DataTable::new(columns)
        .with_id("file_list")
        .with_accessible_name(t("files-a11y-list"))
        .with_row_key(row_key)
        .selectable(true)
        .on_header_click(move |index| header.header_click.set(Some(index)))
}

/// The name cell: the file's icon or preview with its sync emblem, then its
/// name and, for a conflicted file, a warning. (The framework's own text
/// cells paint the other columns.)
fn paint_name_cell(
    row: &ListRow,
    scene: &mut Scene,
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
        scale: cx.scale,
        glyph_px: icon_px,
        glyph_color: ink,
    };
    paint_file_visual(
        scene,
        Affine::IDENTITY,
        tr,
        theme,
        &visual,
        icon_rect,
        &row.entry,
    );
    paint_entry_emblem(
        scene,
        Affine::IDENTITY,
        icon_rect,
        &row.entry,
        cx.scale,
        theme,
    );

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
        Affine::translate((name_x, text_y)),
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
                Affine::IDENTITY,
                Rect::new(x, y, x + side, y + side),
                Icon::TriangleAlert,
                tokens.colors.semantic.warning,
                LUCIDE_DEFAULT_STROKE_PX,
            );
        }
    }
}

/// The header band the table sizes from the theme's caption line.
fn header_height(theme: &Theme) -> f64 {
    let tokens = &theme.tokens;
    text_band(
        f64::from(tokens.metrics.min_pointer_target),
        type_line_height(&tokens.typography, TextRole::Caption),
        f64::from(tokens.spacing.s3),
    )
}

impl ListSlot {
    /// Put this build's entries, selection and sort into the table, making
    /// it first if there is none or the columns changed.
    pub(super) fn sync(&self, inputs: &ListInputs<'_>) {
        let mut state = self.0.borrow_mut();
        state.header_h = header_height(inputs.theme);
        state.fit = Some((inputs.geo, inputs.scale, inputs.text_scale));
        let shape = Shape::for_width(state.width, &inputs.geo, inputs.scale, inputs.text_scale);
        if state.table.is_none() || state.shape != Some(shape) {
            let scroll = state.table.as_ref().map_or(0.0, DataTable::scroll_y);
            let table = build_table(shape, &state.mailbox, &inputs.geo, inputs.text_scale);
            state.table = Some(table);
            state.shape = Some(shape);
            state.viewport = (0.0, 0.0);
            if let Some(table) = state.table.as_mut() {
                table.set_scroll_y(scroll);
            }
        }
        let cx = Rc::new(RowContext {
            scale: inputs.scale,
            geo: inputs.geo,
            thumbs: inputs.thumbs.clone(),
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
        let reset = std::mem::take(&mut state.reset_scroll);
        let ascending = inputs.sort_order == SortOrder::Ascending;
        let Some(table) = state.table.as_mut() else {
            return;
        };
        table.set_rows(rows);
        table.set_selection(inputs.selection.clone());
        // The app hands over entries already in the order it sorted them; the
        // table's comparator keeps that order and only names the column that
        // carries the indicator.
        table.sort_by_with(
            |_: &ListRow, _: &ListRow| Ordering::Equal,
            key_of(inputs.sort_by),
            ascending,
        );
        if reset {
            table.set_scroll_y(0.0);
        }
    }

    /// Scroll back to the top (a folder was opened).
    pub fn reset_scroll(&self) {
        let mut state = self.0.borrow_mut();
        state.reset_scroll = true;
        if let Some(table) = state.table.as_mut() {
            table.set_scroll_y(0.0);
        }
    }

    /// The table's scroll offset.
    #[must_use]
    pub fn scroll_y(&self) -> f32 {
        self.0
            .borrow()
            .table
            .as_ref()
            .map_or(0.0, DataTable::scroll_y)
    }

    /// Scroll to `y` (clamped by the table).
    pub fn set_scroll_y(&self, y: f32) {
        if let Some(table) = self.0.borrow_mut().table.as_mut() {
            table.set_scroll_y(y);
        }
    }

    /// The farthest the list can scroll.
    #[must_use]
    pub fn max_scroll_y(&self) -> f32 {
        self.0
            .borrow()
            .table
            .as_ref()
            .map_or(0.0, DataTable::max_scroll_y)
    }

    /// The table's laid-out rectangle.
    #[must_use]
    pub fn rect(&self) -> Rect {
        self.0
            .borrow()
            .table
            .as_ref()
            .map_or(Rect::ZERO, Widget::get_rect)
    }

    /// Add the table's node under `parent`'s layout and size it to fill.
    pub(super) fn build_layout(&self, taffy: &mut TaffyTree<()>, theme: &Theme) -> Option<NodeId> {
        let mut state = self.0.borrow_mut();
        let table = state.table.as_mut()?;
        // The table is a part of the file view, not a widget of the tree: its
        // node stays out of the tree's widget map.
        let mut scratch = HashMap::new();
        let node = table.build_layout(taffy, theme, &mut scratch);
        let mut style = taffy
            .style(node)
            .cloned()
            .unwrap_or_else(|_| Style::default());
        style.size.height = Dimension::percent(1.0);
        style.flex_grow = 1.0;
        let _ = taffy.set_style(node, style);
        Some(node)
    }

    /// Lay the table out in `node`, and give it the body height and row
    /// height it scrolls by. Returns whether the columns should change with
    /// the new width, so the view is rebuilt.
    pub(super) fn apply_layout(
        &self,
        taffy: &TaffyTree<()>,
        node: NodeId,
        origin: Point,
        row_h: f64,
    ) -> bool {
        let mut state = self.0.borrow_mut();
        let header_h = state.header_h;
        let Some(table) = state.table.as_mut() else {
            return false;
        };
        table.apply_layout(taffy, node, origin);
        let rect = table.get_rect();
        let viewport = ((rect.height() - header_h).max(0.0) as f32, row_h as f32);
        let width = rect.width();
        if viewport != state.viewport {
            state.viewport = viewport;
            if let Some(table) = state.table.as_mut() {
                table.set_viewport(viewport.0, viewport.1);
            }
        }
        state.width = width;
        match state.fit {
            Some((geo, scale, text_scale)) => {
                state.shape != Some(Shape::for_width(width, &geo, scale, text_scale))
            }
            None => false,
        }
    }

    pub(super) fn draw(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let mut state = self.0.borrow_mut();
        let Some(table) = state.table.as_mut() else {
            return;
        };
        if transform == Affine::IDENTITY {
            table.draw(scene, transform, tr, theme, presence);
        } else {
            // The table's cell painters draw in layout coordinates: compose
            // the window transform onto the finished scene instead.
            let mut inner = Scene::new();
            table.draw(&mut inner, Affine::IDENTITY, tr, theme, presence);
            scene.append(&inner, Some(transform));
        }
    }

    /// Forward `event` to the table and report what its callbacks recorded.
    pub(super) fn handle_event(
        &self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> ListOutcome {
        let mailbox = Rc::clone(&self.0.borrow().mailbox);
        {
            let mut state = self.0.borrow_mut();
            if let Some(table) = state.table.as_mut() {
                table.handle_event(event, ctx);
            }
        }
        let state = self.0.borrow();
        let sort_key = mailbox.header_click.take().and_then(|index| {
            let column = state.table.as_ref()?.columns().get(index)?;
            ["name", "size", "modified"]
                .into_iter()
                .find(|key| *key == column.key)
        });
        ListOutcome { sort_key }
    }

    /// Whether `at` is over the table's scrollbar, which the table keeps for
    /// itself (a press there scrolls; a press on a row is the file view's).
    pub(super) fn in_scrollbar(&self, at: Point) -> bool {
        self.0
            .borrow()
            .table
            .as_ref()
            .and_then(DataTable::scrollbar_thumb)
            .is_some_and(|thumb| at.x >= thumb.x0 - 2.0)
    }

    /// Show `selection` in the table (a click was just applied to the view's
    /// own copy).
    pub(super) fn select(&self, selection: &SelectionModel) {
        if let Some(table) = self.0.borrow_mut().table.as_mut() {
            table.set_selection(selection.clone());
        }
    }

    /// Tell the table where the pointer is, so a press that arrives with no
    /// move since the view was rebuilt still lands on the right row.
    pub(super) fn point_at(&self, at: Point, ctx: &mut ui::widget::EventCtx<'_>) {
        let mut state = self.0.borrow_mut();
        let Some(table) = state.table.as_mut() else {
            return;
        };
        let moved = WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(
                at.x * ctx.scale_factor,
                at.y * ctx.scale_factor,
            ),
        };
        let mut handled = false;
        let mut focus = None;
        let mut inner = ui::widget::EventCtx {
            scale_factor: ctx.scale_factor,
            modifiers: ctx.modifiers,
            state: &mut *ctx.state,
            path: ctx.path,
            handled: &mut handled,
            focus_target: &mut focus,
            node_id: ctx.node_id,
            taffy: ctx.taffy,
        };
        table.handle_event(&moved, &mut inner);
    }

    pub(super) fn update(
        &self,
        state: &mut ui::widget::AppModel,
        path: &ui::widget::ModelPath,
    ) -> bool {
        self.0
            .borrow_mut()
            .table
            .as_mut()
            .is_some_and(|table| table.update(state, path))
    }

    pub(super) fn needs_animation_frame(&self) -> bool {
        self.0
            .borrow()
            .table
            .as_ref()
            .is_some_and(Widget::needs_animation_frame)
    }

    pub(super) fn is_layout_dirty(&self) -> bool {
        self.0
            .borrow()
            .table
            .as_ref()
            .is_some_and(Widget::is_layout_dirty)
    }

    pub(super) fn wants_pointer_capture(&self) -> bool {
        self.0
            .borrow()
            .table
            .as_ref()
            .is_some_and(Widget::wants_pointer_capture)
    }

    /// The table swaps its retained cell controls on a scroll; Files has no
    /// such cells, so this never asks for a rebuild.
    pub(super) fn discard_structure_dirty(&self) {
        if let Some(table) = self.0.borrow_mut().table.as_mut() {
            let _ = table.take_structure_dirty();
        }
    }

    /// The header band's height as the table lays it out.
    pub(super) fn header_height(&self) -> f64 {
        self.0.borrow().header_h
    }

    /// Each visible row's rectangle by display index, read from the table's
    /// accessibility export (the table publishes no row geometry) and clipped
    /// to the body below the header band, so a row scrolled half under the
    /// header or the bottom edge is only as large as what shows.
    pub(super) fn row_rects(&self) -> Vec<(usize, Rect)> {
        let state = self.0.borrow();
        let Some(table) = state.table.as_ref() else {
            return Vec::new();
        };
        let table_rect = table.get_rect();
        let body = Rect::new(
            table_rect.x0,
            table_rect.y0 + state.header_h,
            table_rect.x1,
            table_rect.y1,
        );
        let Some(node) = table.accessibility() else {
            return Vec::new();
        };
        node.children
            .iter()
            .filter_map(|row| {
                // A row's `key` is its identity; its place is its position in the set.
                let index = row.pos_in_set?.checked_sub(1)?;
                let rect = row.bounds?.to_rect().intersect(body);
                (rect.width() > 0.0 && rect.height() > 0.0).then_some((index, rect))
            })
            .collect()
    }

    /// The table's accessibility node.
    pub(super) fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        self.0.borrow().table.as_ref()?.accessibility()
    }
}
