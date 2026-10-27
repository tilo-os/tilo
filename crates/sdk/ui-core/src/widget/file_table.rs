//! The file list: a folder's entries as the toolkit's [`DataTable`], with a
//! Name, a Size and a Modified column. It is the one list of files on the
//! desktop: the file manager's folder view and the system file chooser both
//! show it, so a folder reads, sorts, selects and scrolls the same wherever
//! a person meets it.
//!
//! The table paints the header, the rows, the selection lift, the sort
//! indicator, the numeric columns and the scrollbar, and owns scrolling,
//! column resizing, keyboard movement, touch and the accessibility rows. The
//! file list adds what makes it a list of files:
//!
//! * the columns, which shrink and then drop out on a narrow list (the name
//!   always keeps its room) and follow the text scale and the host's zoom;
//! * the name cell, which a row paints itself ([`FileRow::paint_name`]): by
//!   default the file's icon and its name, and in the file manager its
//!   preview, its sync emblem and an in-place rename;
//! * the rows' rectangles, read back from the table's accessibility export,
//!   so a host can resolve a press, a drag or a drop to the row the table
//!   laid out.
//!
//! A [`FileTable`] lives across its host's rebuilds, so its scroll position,
//! its column widths and its scroll anchor survive them. Everything a
//! rebuild changes (rows, selection, sort, zoom) goes in through
//! [`FileTable::sync`].

use std::cell::Cell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::rc::Rc;

use taffy::{Dimension, NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use crate::icons::{LUCIDE_DEFAULT_STROKE_PX, draw_icon, icon_for_path};
use crate::presence::PresenceCtx;
use crate::selection::SelectionModel;
use crate::text_render::{TextKind, TextRenderer};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::Widget;
use crate::widget::data_table::{ColumnDef, ColumnWidth, DataTable, text_band, type_line_height};

/// One row of a file list: a file or a folder.
pub trait FileRow: 'static {
    /// Where the entry is; also the row's identity across rebuilds.
    fn path(&self) -> &Path;
    /// The name the row shows.
    fn name(&self) -> &str;
    fn is_dir(&self) -> bool;
    /// The Size column: a file's size, or what a folder holds once counted
    /// (blank until then).
    fn size_text(&self) -> String;
    /// When the entry last changed, if known.
    fn modified(&self) -> Option<chrono::DateTime<chrono::Local>>;
    /// What kind of thing the entry is, for a list of things that are not
    /// files (an app: Flatpak, `AppImage`, …). A list any of whose rows has a
    /// kind shows a Kind column where it would show when each changed.
    fn kind(&self) -> Option<String> {
        None
    }
    /// Paint the name cell. The default is the file's icon and its name,
    /// which is all a list without previews needs.
    fn paint_name(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        cell: Rect,
        metrics: &FileTableMetrics,
    ) {
        let _ = paint_icon_and_name(
            scene,
            transform,
            theme,
            text_renderer,
            cell,
            metrics,
            NameCell {
                icon: icon_for_path(self.path(), self.is_dir()),
                name: self.name(),
                is_dir: self.is_dir(),
            },
        );
    }
}

/// The file list's measures, from the theme's tokens, scaled by the host's
/// zoom (`scale`). Every host of the list lays it out with these, so a
/// folder's rows and columns are the same size everywhere.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FileTableMetrics {
    /// The host's zoom: 1.0 at the default size.
    pub scale: f64,
    /// The type's scale, which widens the detail columns with it.
    pub text_scale: f64,
    /// One row.
    pub row: f64,
    /// The name cell's leading glyph.
    pub icon: f64,
    /// Between the glyph and the name.
    pub icon_label_gap: f64,
    /// Before the glyph.
    pub icon_lead: f64,
    pub size_col: f64,
    /// The Modified column with the time of day.
    pub modified_col: f64,
    /// The Modified column with the date alone.
    pub date_col: f64,
    /// The width unit the column breakpoints count in.
    pub unit: f64,
    /// The room the scrollbar takes at the table's trailing edge.
    pub scrollbar_gutter: f64,
}

impl FileTableMetrics {
    /// The measures of `theme` at the host's zoom `scale`.
    #[must_use]
    pub fn of(theme: &Theme, scale: f64) -> Self {
        let t = &theme.tokens;
        let sp = &t.spacing;
        let s = |v: f32| f64::from(v);
        let body = s(t.typography.style(TextRole::Body).line_height_px());
        let pointer = s(t.metrics.min_pointer_target);
        Self {
            scale,
            text_scale: f64::from(theme.text_scale),
            row: pointer.max(body + s(sp.s2)),
            icon: s(t.metrics.icon_md),
            icon_label_gap: s(sp.s3),
            icon_lead: s(sp.s1),
            size_col: s(sp.s6) * 4.0,
            modified_col: s(sp.s6) * 7.5,
            date_col: s(sp.s6) * 4.75,
            unit: s(sp.s4) * 8.0,
            scrollbar_gutter: s(t.metrics.scrollbar_gutter),
        }
    }
}

/// What a name cell shows: the entry's glyph and its name.
#[derive(Clone, Copy, Debug)]
pub struct NameCell<'a> {
    pub icon: crate::icons::Icon,
    pub name: &'a str,
    /// A folder's glyph is in primary ink, a file's dimmed.
    pub is_dir: bool,
}

/// Paint a name cell: its icon, then its name elided to the cell, as in
/// every file list. Returns where the name ends, for a row that adds
/// something after it.
pub fn paint_icon_and_name(
    scene: &mut Scene,
    transform: Affine,
    theme: &Theme,
    text_renderer: &mut TextRenderer,
    cell: Rect,
    metrics: &FileTableMetrics,
    shown: NameCell<'_>,
) -> f64 {
    let NameCell { icon, name, is_dir } = shown;
    let tokens = &theme.tokens;
    let pad = f64::from(tokens.spacing.s2);
    let icon_px = metrics.icon * metrics.scale;
    let lead_x = cell.x0 + pad;
    let lead_y = cell.y0 + (cell.height() - icon_px) / 2.0;
    let ink = if is_dir {
        tokens.colors.text_primary
    } else {
        tokens.colors.text_dim
    };
    draw_icon(
        scene,
        transform,
        Rect::new(lead_x, lead_y, lead_x + icon_px, lead_y + icon_px),
        icon,
        ink,
        LUCIDE_DEFAULT_STROKE_PX,
    );
    let name_x = lead_x + icon_px + metrics.icon_label_gap;
    let body = theme.text(TextRole::Body);
    let avail = (cell.x1 - pad - name_x).max(0.0);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a cell width is a few thousand pixels at most"
    )]
    let label = text_renderer.elide_to_width_style(name, avail as f32, &body);
    let (label_w, line_h) = text_renderer.measure_style(&label, None, &body);
    let text_y = cell.y0 + (cell.height() - f64::from(line_h)) / 2.0;
    text_renderer.draw_style(
        scene,
        &label,
        tokens.colors.text_primary,
        transform * Affine::translate((name_x, text_y)),
        None,
        &body,
    );
    name_x + f64::from(label_w)
}

/// How much of a file's modified time its column shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Modified {
    /// The column is dropped.
    Hidden,
    /// The date alone.
    Date,
    /// The date and the time of day.
    DateTime,
}

/// Which optional columns the list is wide enough for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shape {
    pub size: bool,
    pub modified: Modified,
    /// The rows have kinds ([`FileRow::kind`]): the modified column's place
    /// shows them instead. It follows the rows, not the width.
    pub kind: bool,
}

impl Shape {
    /// The columns a list `width` wide shows: the name always has room, so
    /// the detail columns shrink and then drop out on a narrow list (and
    /// sooner with larger type or zoom).
    #[must_use]
    pub fn for_width(width: f64, metrics: &FileTableMetrics) -> Self {
        if width <= 0.0 {
            return Self {
                size: true,
                modified: Modified::DateTime,
                kind: false,
            };
        }
        let unit = metrics.unit * metrics.scale * metrics.text_scale;
        Self {
            size: width > unit * 2.2,
            modified: if width > unit * 4.6 {
                Modified::DateTime
            } else if width > unit * 3.0 {
                Modified::Date
            } else {
                Modified::Hidden
            },
            kind: false,
        }
    }
}

/// The column a list is sorted by, which its header marks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileColumn {
    Name,
    Size,
    Modified,
}

impl FileColumn {
    /// The column's key in the table.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Size => "size",
            Self::Modified => "modified",
        }
    }

    fn of_key(key: &str) -> Option<Self> {
        [Self::Name, Self::Size, Self::Modified]
            .into_iter()
            .find(|column| column.key() == key)
    }
}

/// What the table's callbacks recorded during one event.
#[derive(Default)]
struct Mailbox {
    header_click: Cell<Option<usize>>,
    click: Cell<Option<(usize, bool, bool)>>,
    activate: Cell<Option<usize>>,
}

/// What a host reads after forwarding an event to the list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FileTableOutcome {
    /// A press landed on the header of this column.
    pub sort: Option<FileColumn>,
    /// A row was pressed: its display index, and whether Ctrl and Shift
    /// were held. Only a list that [`FileTable::handles_clicks`] reports it.
    pub click: Option<(usize, bool, bool)>,
    /// A row was opened: double-clicked, or Enter on the selected row.
    pub activate: Option<usize>,
}

/// What a rebuild puts into the list.
pub struct FileTableInputs<'a, R> {
    pub rows: Vec<R>,
    pub selection: &'a SelectionModel,
    pub sort: FileColumn,
    pub ascending: bool,
    pub metrics: FileTableMetrics,
    pub theme: &'a Theme,
}

/// A row as the table holds it: the host's row and the measures its name
/// cell paints with.
struct Held<R> {
    row: R,
    metrics: Rc<FileTableMetrics>,
}

/// The file list a host keeps across its rebuilds.
pub struct FileTable<R: FileRow> {
    table: Option<DataTable<Held<R>>>,
    shape: Option<Shape>,
    mailbox: Rc<Mailbox>,
    id: &'static str,
    accessible_name: String,
    clicks: bool,
    /// The table's width at its last layout, which decides its columns.
    width: f64,
    /// The body height and row height last given to the table.
    viewport: (f32, f32),
    /// The header band, from the theme's type (the table sizes it the same way).
    header_h: f64,
    /// What the columns are fitted with, kept for the layout that re-fits them.
    metrics: Option<FileTableMetrics>,
    /// Scroll back to the top at the next sync.
    reset_scroll: bool,
}

/// A stable identity for a row: its path.
fn row_key<R: FileRow>(held: &Held<R>) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    held.row.path().hash(&mut hasher);
    hasher.finish()
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

fn tr(key: &str) -> String {
    crate::i18n::widget_format(key, &[])
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "column widths are a few hundred pixels"
)]
fn px(width: f64) -> f32 {
    width as f32
}

impl<R: FileRow> FileTable<R> {
    /// A list with the accessibility `id` and `accessible_name` it is known
    /// by. It shows selection; the host decides what a press selects.
    #[must_use]
    pub fn new(id: &'static str, accessible_name: impl Into<String>) -> Self {
        Self {
            table: None,
            shape: None,
            mailbox: Rc::default(),
            id,
            accessible_name: accessible_name.into(),
            clicks: false,
            width: 0.0,
            viewport: (0.0, 0.0),
            header_h: 0.0,
            metrics: None,
            reset_scroll: false,
        }
    }

    /// The list reports a press on a row ([`FileTableOutcome::click`])
    /// instead of selecting it itself, so the host's selection stays the
    /// only one.
    #[must_use]
    pub fn handles_clicks(mut self) -> Self {
        self.clicks = true;
        self
    }

    fn build_table(&self, shape: Shape, metrics: &FileTableMetrics) -> DataTable<Held<R>> {
        let mut columns = vec![
            ColumnDef::new(
                FileColumn::Name.key(),
                tr("widget-filetable-column-name"),
                ColumnWidth::Flex(1.0),
            )
            .sortable(true)
            .with_render(Box::new(
                |held: &Held<R>, scene, transform, theme, text_renderer, cell| {
                    held.row.paint_name(
                        scene,
                        transform,
                        theme,
                        text_renderer,
                        cell,
                        &held.metrics,
                    );
                },
            ))
            .with_accessible_value(|held: &Held<R>| held.row.name().to_owned()),
        ];
        if shape.size {
            columns.push(
                ColumnDef::new(
                    FileColumn::Size.key(),
                    tr("widget-filetable-column-size"),
                    ColumnWidth::Fixed(px(metrics.size_col * metrics.text_scale)),
                )
                .sortable(true)
                .with_kind(TextKind::Numeric)
                .with_text(|held: &Held<R>| held.row.size_text()),
            );
        }
        if shape.kind && shape.modified != Modified::Hidden {
            columns.push(
                ColumnDef::new(
                    "kind",
                    tr("widget-filetable-column-kind"),
                    ColumnWidth::Fixed(px(metrics.date_col * metrics.text_scale)),
                )
                .with_text(|held: &Held<R>| held.row.kind().unwrap_or_default()),
            );
        } else if shape.modified != Modified::Hidden {
            let with_time = shape.modified == Modified::DateTime;
            let width = if with_time {
                metrics.modified_col
            } else {
                metrics.date_col
            };
            columns.push(
                ColumnDef::new(
                    FileColumn::Modified.key(),
                    tr("widget-filetable-column-modified"),
                    ColumnWidth::Fixed(px(width * metrics.text_scale)),
                )
                .sortable(true)
                .with_kind(TextKind::Numeric)
                .with_optional_text(
                    move |held: &Held<R>| {
                        held.row.modified().map(|at| {
                            if with_time {
                                crate::format::timestamp(&at)
                            } else {
                                crate::format::date(at.date_naive())
                            }
                        })
                    },
                    "—",
                ),
            );
        }
        // The table's scrollbar overlays its last column's trailing edge; an
        // empty column of the scrollbar's gutter keeps the last column's
        // figures clear of it.
        columns.push(ColumnDef::new(
            "end",
            "",
            ColumnWidth::Fixed(px(metrics.scrollbar_gutter)),
        ));
        let header = Rc::clone(&self.mailbox);
        let activate = Rc::clone(&self.mailbox);
        let table = DataTable::new(columns)
            .with_id(self.id)
            .with_accessible_name(self.accessible_name.clone())
            .with_row_key(row_key::<R>)
            .selectable(true)
            .on_header_click(move |index| header.header_click.set(Some(index)))
            .on_activate(move |index| activate.activate.set(Some(index)));
        if self.clicks {
            let click = Rc::clone(&self.mailbox);
            table.on_row_click(move |index, ctrl, shift| {
                click.click.set(Some((index, ctrl, shift)));
            })
        } else {
            table
        }
    }

    /// Put this build's rows, selection and sort into the table, making it
    /// first if there is none or the columns changed.
    pub fn sync(&mut self, inputs: FileTableInputs<'_, R>) {
        self.header_h = header_height(inputs.theme);
        self.metrics = Some(inputs.metrics);
        let shape = Shape {
            kind: inputs.rows.iter().any(|row| row.kind().is_some()),
            ..Shape::for_width(self.width, &inputs.metrics)
        };
        if self.table.is_none() || self.shape != Some(shape) {
            let scroll = self.table.as_ref().map_or(0.0, |table| table.scroll_y());
            let mut table = self.build_table(shape, &inputs.metrics);
            table.set_scroll_y(scroll);
            self.table = Some(table);
            self.shape = Some(shape);
            self.viewport = (0.0, 0.0);
        }
        let metrics = Rc::new(inputs.metrics);
        let rows: Vec<Held<R>> = inputs
            .rows
            .into_iter()
            .map(|row| Held {
                row,
                metrics: Rc::clone(&metrics),
            })
            .collect();
        let reset = std::mem::take(&mut self.reset_scroll);
        let Some(table) = self.table.as_mut() else {
            return;
        };
        table.set_rows(rows);
        table.set_selection(inputs.selection.clone());
        // The host hands over rows already in the order it sorted them; the
        // table shows which column that is by.
        table.set_sort_indicator(inputs.sort.key(), inputs.ascending);
        if reset {
            table.set_scroll_y(0.0);
        }
    }

    /// The column widths the person set by dragging an edge, by column key:
    /// what a host keeps so a resized column is where they left it next
    /// time.
    #[must_use]
    pub fn column_widths(&self) -> Vec<(String, f32)> {
        self.table
            .as_ref()
            .map(|table| table.resized_widths())
            .unwrap_or_default()
    }

    /// Lay the columns out at the widths the person set last time. Widths
    /// for columns this table does not show are ignored.
    pub fn set_column_widths(&mut self, widths: impl IntoIterator<Item = (String, f32)>) {
        let Some(table) = self.table.as_mut() else {
            return;
        };
        for (key, px) in widths {
            table.set_column_width(&key, px);
        }
    }

    /// The rows as the table holds them, in display order.
    pub fn rows(&self) -> Vec<&R> {
        self.table
            .as_ref()
            .map(|table| {
                table
                    .view_rows()
                    .into_iter()
                    .map(|held| &held.row)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Scroll back to the top (a folder was opened).
    pub fn reset_scroll(&mut self) {
        self.reset_scroll = true;
        if let Some(table) = self.table.as_mut() {
            table.set_scroll_y(0.0);
        }
    }

    /// The table's scroll offset.
    #[must_use]
    pub fn scroll_y(&self) -> f32 {
        self.table.as_ref().map_or(0.0, |table| table.scroll_y())
    }

    /// Scroll to `y` (clamped by the table).
    pub fn set_scroll_y(&mut self, y: f32) {
        if let Some(table) = self.table.as_mut() {
            table.set_scroll_y(y);
        }
    }

    /// The farthest the list can scroll.
    #[must_use]
    pub fn max_scroll_y(&self) -> f32 {
        self.table
            .as_ref()
            .map_or(0.0, |table| table.max_scroll_y())
    }

    /// Scroll so the row at display `index` is in view.
    pub fn reveal_row(&mut self, index: usize) {
        if let Some(table) = self.table.as_mut() {
            table.reveal_row(index);
        }
    }

    /// The table's laid-out rectangle.
    #[must_use]
    pub fn rect(&self) -> Rect {
        self.table.as_ref().map_or(Rect::ZERO, Widget::get_rect)
    }

    /// Add the table's node to `taffy`, sized to fill its parent.
    pub fn build_layout(&mut self, taffy: &mut TaffyTree<()>, theme: &Theme) -> Option<NodeId> {
        let table = self.table.as_mut()?;
        // The table is a part of its host, not a widget of the tree: its
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
    /// the new width, so the host syncs again.
    pub fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) -> bool {
        let Some(table) = self.table.as_mut() else {
            return false;
        };
        table.apply_layout(taffy, node, origin);
        self.fit(table_rect(self.table.as_ref()))
    }

    /// Lay the table out in `rect` on a layout of its own, for a host that
    /// places its parts itself. Returns what [`Self::apply_layout`] does.
    pub fn place(&mut self, rect: Rect, theme: &Theme) -> bool {
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let Some(node) = self.build_layout(&mut taffy, theme) else {
            return false;
        };
        let Ok(root) = taffy.new_with_children(
            Style {
                size: taffy::Size {
                    width: Dimension::length(px(rect.width())),
                    height: Dimension::length(px(rect.height())),
                },
                ..Default::default()
            },
            &[node],
        ) else {
            return false;
        };
        if taffy
            .compute_layout(
                root,
                taffy::Size {
                    width: taffy::AvailableSpace::MaxContent,
                    height: taffy::AvailableSpace::MaxContent,
                },
            )
            .is_err()
        {
            return false;
        }
        self.apply_layout(&taffy, node, rect.origin())
    }

    /// Hand the table the viewport `rect` gives it; report whether its
    /// columns no longer fit the width.
    fn fit(&mut self, rect: Rect) -> bool {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a list's height and row height are screen pixels"
        )]
        let viewport = (
            (rect.height() - self.header_h).max(0.0) as f32,
            self.metrics.map_or(0.0, |m| m.row * m.scale) as f32,
        );
        if viewport != self.viewport {
            self.viewport = viewport;
            if let Some(table) = self.table.as_mut() {
                table.set_viewport(viewport.0, viewport.1);
            }
        }
        self.width = rect.width();
        let kind = self.shape.is_some_and(|shape| shape.kind);
        self.metrics.is_some_and(|metrics| {
            self.shape
                != Some(Shape {
                    kind,
                    ..Shape::for_width(self.width, &metrics)
                })
        })
    }

    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if let Some(table) = self.table.as_mut() {
            table.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    /// Forward `event` to the table and report what its callbacks recorded.
    pub fn handle_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) -> FileTableOutcome {
        if let Some(table) = self.table.as_mut() {
            table.handle_event(event, ctx);
        }
        self.take_outcome()
    }

    /// What the table's callbacks recorded since the last read.
    pub fn take_outcome(&mut self) -> FileTableOutcome {
        let sort = self.mailbox.header_click.take().and_then(|index| {
            let column = self.table.as_ref()?.columns().get(index)?;
            FileColumn::of_key(&column.key)
        });
        FileTableOutcome {
            sort,
            click: self.mailbox.click.take(),
            activate: self.mailbox.activate.take(),
        }
    }

    /// Whether `at` is over the table's scrollbar, which the table keeps for
    /// itself.
    #[must_use]
    pub fn in_scrollbar(&self, at: Point) -> bool {
        self.table
            .as_ref()
            .and_then(|table| table.scrollbar_thumb())
            .is_some_and(|thumb| at.x >= thumb.x0 - 2.0)
    }

    /// Show `selection` in the table.
    pub fn select(&mut self, selection: &SelectionModel) {
        if let Some(table) = self.table.as_mut() {
            table.set_selection(selection.clone());
        }
    }

    /// The selection as the table holds it (keyboard movement changes it).
    #[must_use]
    pub fn selection(&self) -> Option<&SelectionModel> {
        self.table.as_ref().map(|table| table.selection())
    }

    /// Tell the table where the pointer is, so a press that arrives with no
    /// move since the host was rebuilt still lands on the right row.
    pub fn point_at(&mut self, at: Point, ctx: &mut crate::widget::EventCtx<'_>) {
        let Some(table) = self.table.as_mut() else {
            return;
        };
        let moved =
            crate::synthetic_input::cursor_moved(at.x * ctx.scale_factor, at.y * ctx.scale_factor);
        let mut handled = false;
        let mut focus = None;
        let mut inner = crate::widget::EventCtx {
            scale_factor: ctx.scale_factor,
            modifiers: ctx.modifiers,
            state: ctx.state.reborrow(),
            path: ctx.path,
            handled: &mut handled,
            focus_target: &mut focus,
            node_id: ctx.node_id,
            taffy: ctx.taffy,
        };
        table.handle_event(&moved, &mut inner);
    }

    pub fn update(
        &mut self,
        state: &crate::widget::AppModel,
        path: &crate::widget::ModelPath,
    ) -> bool {
        self.table
            .as_mut()
            .is_some_and(|table| table.update(state, path))
    }

    #[must_use]
    pub fn needs_animation_frame(&self) -> bool {
        self.table
            .as_ref()
            .is_some_and(Widget::needs_animation_frame)
    }

    #[must_use]
    pub fn is_layout_dirty(&self) -> bool {
        self.table.as_ref().is_some_and(Widget::is_layout_dirty)
    }

    #[must_use]
    pub fn wants_pointer_capture(&self) -> bool {
        self.table
            .as_ref()
            .is_some_and(Widget::wants_pointer_capture)
    }

    /// The table swaps its retained cell controls on a scroll; a file list
    /// has no such cells, so this never asks for a rebuild.
    pub fn discard_structure_dirty(&mut self) {
        if let Some(table) = self.table.as_mut() {
            let _ = table.take_structure_dirty();
        }
    }

    pub fn set_focus(&mut self, focused: bool) {
        if let Some(table) = self.table.as_mut() {
            table.set_focus(focused);
        }
    }

    /// The header band's height as the table lays it out.
    #[must_use]
    pub fn header_height(&self) -> f64 {
        self.header_h
    }

    /// Each visible row's rectangle by display index, read from the table's
    /// accessibility export (the table publishes no row geometry) and
    /// clipped to the body below the header band, so a row scrolled half
    /// under the header or the bottom edge is only as large as what shows.
    #[must_use]
    pub fn row_rects(&self) -> Vec<(usize, Rect)> {
        let Some(table) = self.table.as_ref() else {
            return Vec::new();
        };
        let table_rect = table.get_rect();
        let body = Rect::new(
            table_rect.x0,
            table_rect.y0 + self.header_h,
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
    #[must_use]
    pub fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        self.table.as_ref()?.accessibility()
    }

    /// Run an assistive technology's `action` on the table's row at
    /// `key_path` (empty: the table itself).
    pub fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        key_path: &[u64],
        state: &mut crate::widget::Edits<'_>,
        path: &crate::widget::ModelPath,
    ) -> bool {
        self.table
            .as_mut()
            .is_some_and(|table| table.perform_action(action, key_path, state, path))
    }
}

fn table_rect<R>(table: Option<&DataTable<R>>) -> Rect
where
    R: 'static,
{
    table.map_or(Rect::ZERO, Widget::get_rect)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Plain(std::path::PathBuf, bool);

    impl FileRow for Plain {
        fn path(&self) -> &Path {
            &self.0
        }
        fn name(&self) -> &str {
            self.0.file_name().and_then(|n| n.to_str()).unwrap_or("")
        }
        fn is_dir(&self) -> bool {
            self.1
        }
        fn size_text(&self) -> String {
            String::new()
        }
        fn modified(&self) -> Option<chrono::DateTime<chrono::Local>> {
            None
        }
    }

    #[test]
    fn a_narrow_list_keeps_the_name_and_drops_the_details_first() {
        let theme = Theme::default();
        let metrics = FileTableMetrics::of(&theme, 1.0);
        let unit = metrics.unit * metrics.text_scale;
        let wide = Shape::for_width(unit * 6.0, &metrics);
        assert!(wide.size);
        assert_eq!(wide.modified, Modified::DateTime);
        let middling = Shape::for_width(unit * 3.5, &metrics);
        assert_eq!(middling.modified, Modified::Date);
        let narrow = Shape::for_width(unit * 2.0, &metrics);
        assert!(!narrow.size);
        assert_eq!(narrow.modified, Modified::Hidden);
    }

    #[test]
    fn the_list_lays_out_its_rows_and_reports_them() {
        let theme = Theme::default();
        let metrics = FileTableMetrics::of(&theme, 1.0);
        let mut table = FileTable::<Plain>::new("file_list", "Files");
        let selection = SelectionModel::new();
        let rows = |n: usize| {
            (0..n)
                .map(|i| Plain(std::path::PathBuf::from(format!("/home/a/f{i}")), i == 0))
                .collect::<Vec<_>>()
        };
        table.sync(FileTableInputs {
            rows: rows(3),
            selection: &selection,
            sort: FileColumn::Name,
            ascending: true,
            metrics,
            theme: &theme,
        });
        table.place(Rect::new(0.0, 0.0, 600.0, 400.0), &theme);
        let rects = table.row_rects();
        assert_eq!(rects.len(), 3, "every row is laid out: {rects:?}");
        assert!(
            rects
                .windows(2)
                .all(|pair| pair[0].1.y1 <= pair[1].1.y0 + 0.5)
        );
        assert_eq!(table.rows().len(), 3);
    }
}
