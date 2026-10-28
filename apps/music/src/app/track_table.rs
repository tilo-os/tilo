//! The track list every view shares (songs, an album, a playlist, liked
//! songs, downloads, the queue) as the toolkit's [`DataTable`], embedded in
//! the page the app paints.
//!
//! The table owns its rows: their lift under the pointer, the press that
//! plays one, the right-click that opens its menu and the row's actions,
//! which are ordinary [`Button`]s mounted for the row under the pointer. The
//! page scrolls as a whole, so the table is windowed by the app's scroll
//! ([`DataTable::host_scrolled`]). What a row asks for comes back as a
//! [`RowEvent`] the app turns into its own action.

use std::cell::RefCell;
use std::rc::Rc;

use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::text_render::TextKind;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::data_table::{CellState, ColumnDef, ColumnWidth, DataTable};
use ui::widget::{Button, ButtonFace, ControlSize, Row, Widget};
use vello::kurbo::{Point, Rect};

use super::page_part::Placed;
use super::render::{paint_equalizer, square_around, text_line, text_right_in_band};

/// A song's offline state, as its row shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Offline {
    Streamed,
    /// Per cent downloaded.
    Downloading(u8),
    Cached,
    Failed,
}

/// One row of a track table: everything its cells show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TrackCell {
    pub id: String,
    pub title: String,
    pub artist: String,
    /// The second column's text: the album, or a download's status.
    pub second: String,
    pub number: u32,
    pub duration: String,
    pub playback: Playback,
    pub liked: bool,
    pub offline: Offline,
    /// Why the download failed, in full: the retry control's tooltip.
    pub offline_error: Option<String>,
    /// A file opened from disk: it is neither liked nor downloaded.
    pub local: bool,
}

/// Whether a row is the track playing now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Playback {
    Idle,
    /// The current track, paused.
    Paused,
    Playing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Second {
    Album,
    /// A download's status in words (the downloads view).
    Status,
}

/// The actions a row offers under the pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Actions {
    /// Like and a compact menu for track management.
    Track,
    Download,
}

impl Actions {
    pub(super) fn count(self) -> usize {
        match self {
            Self::Track => 2,
            Self::Download => 1,
        }
    }
}

/// How a table is built: its columns and their measured widths. A table is
/// rebuilt when this changes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Shape {
    pub numbers: bool,
    pub second: Option<Second>,
    /// The title column's width beside a second column: as wide as its
    /// widest line. Alone, the title takes the room.
    pub title_width: Option<f32>,
    pub header: bool,
    pub actions: Actions,
    pub row_height: f32,
    pub index_width: f32,
    pub duration_width: f32,
    pub action_width: f32,
    /// Rows spring to their new places when the songs change: the queue,
    /// whose order the person changes in place.
    pub moves: bool,
}

/// What a row asked for; the index is the row's place in the table.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum RowEvent {
    Play(usize),
    /// The row was added to or taken out of the selection (Ctrl or Shift
    /// with the press): nothing plays.
    Select(usize),
    /// A drag began on the row: the selected songs go with it.
    Drag(usize),
    Menu(usize, Rect),
    Like(usize),
    Offline(usize),
    More(usize),
}

type Events = Rc<RefCell<Vec<RowEvent>>>;

/// A track table and the host that lays it out in the painted page.
pub(super) struct TrackTableHost {
    pub(super) host: Placed<DataTable<TrackCell>>,
    shape: Shape,
    rows: Vec<TrackCell>,
    selected: Option<usize>,
    events: Events,
    /// A row a capture shows under the pointer.
    staged_pointer: Option<usize>,
}

impl TrackTableHost {
    pub(super) fn new(shape: Shape, theme: &Theme) -> Self {
        let events: Events = Rc::default();
        Self {
            host: Placed::new(build_table(shape, theme, &events)),
            shape,
            rows: Vec::new(),
            selected: None,
            events,
            staged_pointer: None,
        }
    }

    /// Bring the table up to date with this frame's rows and the row that
    /// stays lifted (a row whose menu is open, a membership being shown).
    pub(super) fn update(
        &mut self,
        shape: Shape,
        rows: Vec<TrackCell>,
        selected: Option<usize>,
        theme: &Theme,
    ) {
        if shape != self.shape {
            self.host.replace(build_table(shape, theme, &self.events));
            self.shape = shape;
            self.rows.clear();
            self.host.widget_mut().set_rows(rows.clone());
            self.rows = rows;
        } else if rows != self.rows {
            self.host.widget_mut().set_rows(rows.clone());
            self.rows = rows;
        }
        if selected != self.selected || selected.is_some() {
            // A revealed row is lifted alone; when the reveal ends, the
            // person's own selection is theirs again.
            let mut selection = ui::selection::SelectionModel::new();
            if let Some(row) = selected {
                selection.select_only(row);
            }
            self.host.widget_mut().set_selection(selection);
            self.selected = selected;
        }
        self.host.unplace();
    }

    /// Whether rows are on their way after the songs changed.
    pub(super) fn rows_moving(&self) -> bool {
        ui::widget::Widget::needs_animation_frame(self.host.widget())
    }

    /// The rows selected, by their place in the table.
    pub(super) fn selected_rows(&self) -> Vec<usize> {
        self.host.widget().selected_rows()
    }

    /// The songs selected, in the table's order.
    pub(super) fn selected_ids(&self) -> Vec<String> {
        self.selected_rows()
            .into_iter()
            .filter_map(|row| self.rows.get(row).map(|cell| cell.id.clone()))
            .collect()
    }

    /// Select the rows of the songs `ids`, as they were selected before; a
    /// song no longer in the table is left out.
    pub(super) fn select_ids(&mut self, ids: &[String]) {
        let mut selection = ui::selection::SelectionModel::new();
        for (row, cell) in self.rows.iter().enumerate() {
            if ids.contains(&cell.id) {
                selection.toggle(row);
            }
        }
        self.host.widget_mut().set_selection(selection);
    }

    pub(super) fn rows(&self) -> &[TrackCell] {
        &self.rows
    }

    /// Lay the table out from `top` across `[left, right]` and paint the
    /// part of it inside `clip` (the page's viewport). Returns the y below
    /// the table.
    pub(super) fn place(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        (left, right): (f64, f64),
        top: f64,
        clip: Rect,
    ) -> f64 {
        // The table learns its header and row heights from the theme when
        // it is laid out; a first layout (or one staged for a capture, which
        // brings the pointer over a row) is solved again against them.
        let mut bottom = top;
        for pass in 0..2 {
            let table = self.host.widget_mut();
            let header = table.header_height();
            bottom = top + table.content_height();
            let shown_top = top.max(clip.y0);
            // Leave breathing room above the transport and never paint the
            // beginning of a row that cannot fit in full.
            let limit = bottom.min(clip.y1 - f64::from(ctx.theme.tokens.spacing.rhythm.related));
            let height = f64::from(self.shape.row_height);
            let complete = ((limit - top - header) / height).floor().max(0.0);
            let shown_bottom = top + header + complete * height;
            // Less than a header and a sliver of a row in view: nothing to
            // show.
            if shown_bottom - shown_top <= header + 1.0 {
                return bottom;
            }
            table.set_host_window(shown_top - top, shown_bottom - shown_top - header);
            self.host
                .layout(Rect::new(left, shown_top, right, shown_bottom), ctx);
            let staged = (pass == 0)
                .then(|| self.staged_pointer.take())
                .flatten()
                .is_some_and(|row| self.point_at_row(row));
            let settled =
                (top + self.host.widget().content_height() - bottom).abs() < 0.5 && !staged;
            if settled {
                break;
            }
        }
        self.host.draw(ctx, clip);
        bottom
    }

    /// For a capture: show row `index` as it is under the pointer, with its
    /// actions, in the next frame.
    pub(super) fn stage_pointer(&mut self, index: usize) {
        self.staged_pointer = Some(index);
    }

    /// Bring the pointer over row `index`; whether it is in view.
    fn point_at_row(&mut self, index: usize) -> bool {
        let table = self.host.widget();
        let Some(row) = table.row_rect(index) else {
            return false;
        };
        let at = Point::new(row.x0 + row.width() * 0.3, row.center().y);
        self.host.widget_mut().set_pointer_position(at);
        true
    }

    /// What the rows asked for since the last call.
    pub(super) fn take_events(&self) -> Vec<RowEvent> {
        std::mem::take(&mut *self.events.borrow_mut())
    }

    /// Where row `index` stands, when it is in view.
    pub(super) fn row_rect(&self, index: usize) -> Option<Rect> {
        let table = self.host.visible()?;
        let rect = table.row_rect(index)?;
        let body_top = table.get_rect().y0 + table.header_height();
        (rect.y1 > body_top && rect.y0 < table.get_rect().y1).then_some(rect)
    }

    /// Where the control `name` ("like", "offline", "more")
    /// of row `index` stands, while the row shows its actions.
    #[cfg(test)]
    pub(super) fn control_rect(&self, index: usize, name: &str) -> Option<Rect> {
        let id = format!("music.track.{index}.{name}");
        self.host
            .widget()
            .children()
            .into_iter()
            .find_map(|cell| ui::widget::find_test_id(cell, &id))
            .map(Widget::get_rect)
    }
}

/// The space a cell keeps on each side of its text.
fn cell_pad(theme: &Theme) -> f64 {
    f64::from(theme.tokens.spacing.s2)
}

fn build_table(shape: Shape, theme: &Theme, events: &Events) -> DataTable<TrackCell> {
    let pad = cell_pad(theme) as f32;
    let mut columns = Vec::new();
    if shape.numbers {
        columns.push(
            ColumnDef::new(
                "index",
                "",
                ColumnWidth::Fixed(shape.index_width + 2.0 * pad),
            )
            .with_kind(TextKind::Numeric)
            .with_text_render(Box::new(paint_index)),
        );
    }
    columns.push(
        ColumnDef::new(
            "title",
            "Title",
            shape.title_width.map_or(ColumnWidth::Flex(1.0), |width| {
                ColumnWidth::Fixed(width + 2.0 * pad)
            }),
        )
        .with_text_render(Box::new(paint_title))
        .with_accessible_value(|row: &TrackCell| format!("{}, {}", row.title, row.artist)),
    );
    match shape.second {
        Some(Second::Album) => columns.push(
            ColumnDef::new("album", "Album", ColumnWidth::Flex(1.0))
                .with_text(|row: &TrackCell| row.second.clone()),
        ),
        Some(Second::Status) => columns.push(
            ColumnDef::new("status", "Status", ColumnWidth::Flex(1.0))
                .with_text_render(Box::new(paint_status))
                .with_accessible_value(|row: &TrackCell| row.second.clone()),
        ),
        None => {}
    }
    let actions = shape.actions;
    let sink = Rc::clone(events);
    let action_column = ColumnDef::new(
        "actions",
        "",
        ColumnWidth::Fixed(shape.action_width * actions.count() as f32 + 2.0 * pad),
    )
    .with_text_render(Box::new(
        move |row, scene, transform, text, cell, theme, state| {
            if !state.hovered && actions != Actions::Download {
                paint_quiet_actions(&mut PaintCtx::new(scene, transform, text, theme), row, cell);
            }
        },
    ))
    .with_widget(move |row: &TrackCell, index| action_buttons(row, index, actions, &sink));
    // A download's one action (cancel, remove, retry) stays in view; a
    // song's actions show under the pointer over its quiet marks.
    columns.push(if actions == Actions::Download {
        action_column
    } else {
        action_column.revealed_on_hover()
    });
    columns.push(
        ColumnDef::new(
            "time",
            "Time",
            ColumnWidth::Fixed(shape.duration_width + 2.0 * pad),
        )
        .with_kind(TextKind::Numeric)
        .with_text(|row: &TrackCell| row.duration.clone()),
    );

    let play = Rc::clone(events);
    let drag = Rc::clone(events);
    let menu = Rc::clone(events);
    let mut table = DataTable::new(columns)
        .with_id("music.tracks")
        .with_accessible_name("Songs")
        .selectable(true)
        .host_scrolled()
        .with_row_height(shape.row_height)
        .on_row_click(move |row, ctrl, shift| {
            // A press plays the song; with Ctrl or Shift it selects instead,
            // for several songs to drag onto a playlist at once.
            play.borrow_mut().push(if ctrl || shift {
                RowEvent::Select(row)
            } else {
                RowEvent::Play(row)
            });
        })
        .on_row_drag_begin(move |row, _, _| drag.borrow_mut().push(RowEvent::Drag(row)))
        .on_row_context_menu(move |row, _, rect| {
            menu.borrow_mut().push(RowEvent::Menu(row, rect));
        });
    if !(shape.header && shape.second.is_some()) {
        table = table.without_header();
    }
    if shape.moves {
        table = table.with_row_key(|row: &TrackCell| {
            use std::hash::{BuildHasher, BuildHasherDefault, DefaultHasher};
            BuildHasherDefault::<DefaultHasher>::default().hash_one(&row.id)
        });
    }
    table
}

/// The index cell: Play under the pointer, the equalizer for the playing
/// track, else the track's number.
fn paint_index(
    row: &TrackCell,
    scene: &mut vello::Scene,
    transform: vello::kurbo::Affine,
    text: &mut ui::text_render::TextRenderer,
    cell: Rect,
    theme: &Theme,
    state: CellState,
) {
    let ctx = &mut PaintCtx::new(scene, transform, text, theme);
    let x1 = cell.x1 - cell_pad(theme);
    if state.hovered {
        let side = f64::from(theme.tokens.metrics.icon_sm);
        Icon::Play.draw(
            ctx.scene,
            ctx.transform,
            square_around(Point::new(x1 - side / 2.0, cell.center().y), side),
            theme.text_primary,
        );
    } else if row.playback != Playback::Idle {
        paint_equalizer(
            ctx,
            row.playback == Playback::Playing,
            Point::new(x1, cell.center().y),
        );
    } else {
        let numeric = theme.text(TextRole::Caption).numeric();
        text_right_in_band(
            ctx,
            &row.number.to_string(),
            &numeric,
            theme.text_tertiary,
            x1,
            cell,
        );
    }
}

/// The song over its artist, centred on the row as one block; the playing
/// song's title is strong.
fn paint_title(
    row: &TrackCell,
    scene: &mut vello::Scene,
    transform: vello::kurbo::Affine,
    text: &mut ui::text_render::TextRenderer,
    cell: Rect,
    theme: &Theme,
    _state: CellState,
) {
    let ctx = &mut PaintCtx::new(scene, transform, text, theme);
    let title_style = if row.playback == Playback::Idle {
        theme.text(TextRole::Body)
    } else {
        theme.text(TextRole::Body).strong()
    };
    let caption = theme.text(TextRole::Caption);
    let line =
        |ctx: &mut PaintCtx<'_>, style| f64::from(ctx.text.measure_style("Ag", None, style).1);
    let block = line(ctx, &title_style) + line(ctx, &caption);
    let pad = cell_pad(theme);
    let x = cell.x0 + pad;
    let width = (cell.width() - 2.0 * pad).max(0.0);
    let top = cell.y0 + (cell.height() - block) / 2.0;
    let title_h = text_line(
        ctx,
        &row.title,
        &title_style,
        theme.text_primary,
        Point::new(x, top),
        width,
    );
    text_line(
        ctx,
        &row.artist,
        &caption,
        theme.text_dim,
        Point::new(x, top + title_h),
        width,
    );
}

/// A download's status in words, in danger ink when it failed.
fn paint_status(
    row: &TrackCell,
    scene: &mut vello::Scene,
    transform: vello::kurbo::Affine,
    text: &mut ui::text_render::TextRenderer,
    cell: Rect,
    theme: &Theme,
    _state: CellState,
) {
    let ctx = &mut PaintCtx::new(scene, transform, text, theme);
    let caption = theme.text(TextRole::Caption);
    let ink = if row.offline == Offline::Failed {
        theme.tokens.colors.semantic.danger
    } else {
        theme.text_dim
    };
    let pad = cell_pad(theme);
    let h = f64::from(ctx.text.measure_style("Ag", None, &caption).1);
    let top = ui::text_metrics::centered_text_top(cell.y0, cell.height(), h);
    text_line(
        ctx,
        &row.second,
        &caption,
        ink,
        Point::new(cell.x0 + pad, top),
        (cell.width() - 2.0 * pad).max(0.0),
    );
}

/// Where action `k` stands in the actions cell: the buttons run from the
/// cell's leading edge, each a small control's square.
fn action_slot(cell: Rect, k: usize, theme: &Theme) -> Rect {
    let side = action_side(theme);
    let x0 = cell.x0 + side * k as f64;
    let mid = cell.center().y;
    Rect::new(x0, mid - side / 2.0, x0 + side, mid + side / 2.0)
}

/// One action button's side.
pub(super) fn action_side(theme: &Theme) -> f64 {
    ButtonFace::height(ControlSize::Small, theme)
}

/// At rest a liked song keeps its muted heart; other actions stay in More.
fn paint_quiet_actions(ctx: &mut PaintCtx<'_>, row: &TrackCell, cell: Rect) {
    if row.local {
        return;
    }
    let theme = ctx.theme;
    let glyph = |slot: Rect| square_around(slot.center(), f64::from(theme.tokens.metrics.icon_sm));
    let like = 0;
    if row.liked {
        Icon::Heart.draw_filled(
            ctx.scene,
            ctx.transform,
            glyph(action_slot(cell, like, theme)),
            theme.text_dim,
        );
    }
}

/// The offline button of a row: its glyph, its name and what a press asks.
fn offline_button(row: &TrackCell) -> (Icon, &'static str) {
    match row.offline {
        Offline::Failed => (Icon::Refresh, "Retry download"),
        Offline::Downloading(_) => (Icon::X, "Cancel download"),
        Offline::Cached => (Icon::Trash2, "Remove download"),
        Offline::Streamed => (Icon::Download, "Keep offline"),
    }
}

/// The row's actions as buttons, in the order the quiet marks stand.
fn action_buttons(
    row: &TrackCell,
    index: usize,
    actions: Actions,
    events: &Events,
) -> Box<dyn Widget> {
    let button = |name: &str, icon: Icon, label: &str, event: RowEvent| {
        let sink = Rc::clone(events);
        Button::icon_only(&format!("music.track.{index}.{name}"), icon, label)
            .control_size(ControlSize::Small)
            .on_effect(move || sink.borrow_mut().push(event))
    };
    let mut cluster = Row::new().no_wrap().with_gap(0.0);
    if actions != Actions::Download {
        cluster = cluster.push(Box::new(
            button(
                "like",
                Icon::Heart,
                if row.liked { "Unlike" } else { "Like" },
                RowEvent::Like(index),
            )
            .toggled(row.liked)
            .with_enabled(!row.local),
        ));
    }
    if actions != Actions::Download {
        return Box::new(cluster.push(Box::new(button(
            "more",
            Icon::MoreHorizontal,
            "Track actions",
            RowEvent::More(index),
        ))));
    }
    let (icon, label) = offline_button(row);
    let mut offline =
        button("offline", icon, label, RowEvent::Offline(index)).with_enabled(!row.local);
    if let Some(error) = &row.offline_error {
        // The row has no room for the cause; the control names it.
        offline = offline.tooltip(error.clone());
    }
    cluster = cluster.push(Box::new(offline));
    Box::new(cluster)
}
