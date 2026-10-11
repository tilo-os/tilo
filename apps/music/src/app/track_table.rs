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
use ui::presence::PresenceCtx;
use ui::text_render::TextKind;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::data_table::{CellState, ColumnDef, ColumnWidth, DataTable};
use ui::widget::{Button, ButtonFace, ControlSize, Embedded, Row, Widget};
use vello::kurbo::{Point, Rect};
use winit::event::WindowEvent;

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
    pub place: Place,
}

/// Whether a row is the track playing now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Playback {
    Idle,
    /// The current track, paused.
    Paused,
    Playing,
}

/// Where a row stands in its table: what it can move past.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Place {
    Only,
    First,
    Middle,
    Last,
}

impl Place {
    pub(super) fn of(index: usize, len: usize) -> Self {
        match (index == 0, index + 1 >= len) {
            (true, true) => Self::Only,
            (true, false) => Self::First,
            (false, true) => Self::Last,
            (false, false) => Self::Middle,
        }
    }
}

/// The second column of a table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Second {
    Album,
    /// A download's status in words (the downloads view).
    Status,
}

/// The actions a row offers under the pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Actions {
    /// Like and keep offline.
    Track,
    /// Move up, move down and remove, then like and keep offline.
    Reorder {
        /// The rows are a playlist's (not the queue's).
        playlist: bool,
    },
    /// The download's one action.
    Download,
}

impl Actions {
    fn count(self) -> usize {
        match self {
            Self::Track => 2,
            Self::Reorder { .. } => 5,
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
}

/// What a row asked for; the index is the row's place in the table.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum RowEvent {
    Play(usize),
    Menu(usize, Rect),
    Like(usize),
    Offline(usize),
    MoveUp(usize),
    MoveDown(usize),
    Remove(usize),
}

type Events = Rc<RefCell<Vec<RowEvent>>>;

/// A track table and the host that lays it out in the painted page.
pub(super) struct TrackTableHost {
    host: Embedded<DataTable<TrackCell>>,
    shape: Shape,
    rows: Vec<TrackCell>,
    selected: Option<usize>,
    events: Events,
    /// Whether the table was placed (some of it is in view) this frame.
    placed: bool,
    /// A row a capture shows under the pointer.
    staged_pointer: Option<usize>,
}

impl TrackTableHost {
    pub(super) fn new(shape: Shape, theme: &Theme) -> Self {
        let events: Events = Rc::default();
        Self {
            host: Embedded::new(build_table(shape, theme, &events)),
            shape,
            rows: Vec::new(),
            selected: None,
            events,
            placed: false,
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
            self.host = Embedded::new(build_table(shape, theme, &self.events));
            self.shape = shape;
            self.rows.clear();
            self.host.widget_mut().set_rows(rows.clone());
            self.rows = rows;
        } else if rows != self.rows {
            self.host.widget_mut().set_rows(rows.clone());
            self.rows = rows;
        }
        if selected != self.selected || selected.is_some() {
            let mut selection = ui::selection::SelectionModel::new();
            if let Some(row) = selected {
                selection.select_only(row);
            }
            self.host.widget_mut().set_selection(selection);
            self.selected = selected;
        }
        self.placed = false;
    }

    pub(super) fn visible_widget(&self) -> Option<&dyn Widget> {
        self.placed.then_some(self.host.widget())
    }

    pub(super) fn visible_widget_mut(&mut self) -> Option<&mut (dyn Widget + '_)> {
        if self.placed {
            Some(self.host.widget_mut())
        } else {
            None
        }
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
            let shown_bottom = bottom.min(clip.y1);
            // Less than a header and a sliver of a row in view: nothing to
            // show.
            if shown_bottom - shown_top <= header + 1.0 {
                return bottom;
            }
            table.set_host_window(shown_top - top, shown_bottom - shown_top - header);
            self.host.layout(
                Rect::new(left, shown_top, right, shown_bottom),
                ctx.theme,
                ctx.text,
            );
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
        self.host.draw(
            ctx.scene,
            ctx.transform,
            ctx.text,
            ctx.theme,
            &PresenceCtx::default(),
        );
        self.placed = true;
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

    /// Route one window event to the table; a table out of view takes none.
    pub(super) fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if !self.placed {
            return;
        }
        self.host.handle_event(
            event,
            ctx.scale_factor,
            ctx.modifiers,
            &mut ctx.state,
            ctx.path,
            ctx.handled,
        );
    }

    /// What the rows asked for since the last call.
    pub(super) fn take_events(&self) -> Vec<RowEvent> {
        std::mem::take(&mut *self.events.borrow_mut())
    }

    /// Where row `index` stands, when it is in view.
    pub(super) fn row_rect(&self, index: usize) -> Option<Rect> {
        if !self.placed {
            return None;
        }
        let table = self.host.widget();
        let rect = table.row_rect(index)?;
        let body_top = table.get_rect().y0 + table.header_height();
        (rect.y1 > body_top && rect.y0 < table.get_rect().y1).then_some(rect)
    }

    /// Where the control `name` ("like", "offline", "up", "down", "remove")
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

    /// Whether a control of a row (not the row itself) is under `point`,
    /// and what it is called.
    pub(super) fn tooltip_at(&self, point: Point) -> Option<String> {
        fn find(widget: &dyn Widget, point: Point) -> Option<String> {
            for child in widget.children() {
                if let Some(found) = find(child, point) {
                    return Some(found);
                }
            }
            (widget.get_rect().contains(point))
                .then(|| widget.tooltip().map(str::to_owned))
                .flatten()
        }
        if !self.placed {
            return None;
        }
        self.host
            .widget()
            .children()
            .into_iter()
            .find_map(|cell| find(cell, point))
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
                paint_quiet_actions(
                    &mut PaintCtx::new(scene, transform, text, theme),
                    row,
                    actions,
                    cell,
                );
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
    let menu = Rc::clone(events);
    let mut table = DataTable::new(columns)
        .with_id("music.tracks")
        .with_accessible_name("Songs")
        .selectable(true)
        .host_scrolled()
        .with_row_height(shape.row_height)
        .on_row_click(move |row, _, _| play.borrow_mut().push(RowEvent::Play(row)))
        .on_row_context_menu(move |row, _, rect| {
            menu.borrow_mut().push(RowEvent::Menu(row, rect));
        });
    if !(shape.header && shape.second.is_some()) {
        table = table.without_header();
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

/// A row at rest: only what is already so shows, muted. A liked song keeps
/// its mark, a downloaded or failed one its state, a download its progress.
fn paint_quiet_actions(ctx: &mut PaintCtx<'_>, row: &TrackCell, actions: Actions, cell: Rect) {
    if row.local {
        return;
    }
    let theme = ctx.theme;
    let glyph = |slot: Rect| square_around(slot.center(), f64::from(theme.tokens.metrics.icon_sm));
    let (like, offline) = match actions {
        Actions::Reorder { .. } => (3, 4),
        Actions::Track | Actions::Download => (0, 1),
    };
    if row.liked {
        Icon::Heart.draw_filled(
            ctx.scene,
            ctx.transform,
            glyph(action_slot(cell, like, theme)),
            theme.text_dim,
        );
    }
    let slot = action_slot(cell, offline, theme);
    match row.offline {
        Offline::Failed => Icon::TriangleAlert.draw(
            ctx.scene,
            ctx.transform,
            glyph(slot),
            theme.tokens.colors.semantic.danger,
        ),
        Offline::Cached => Icon::Check.draw(ctx.scene, ctx.transform, glyph(slot), theme.text_dim),
        Offline::Downloading(percent) => {
            let numeric = theme.text(TextRole::Caption).numeric();
            text_right_in_band(
                ctx,
                &format!("{percent}%"),
                &numeric,
                theme.text_dim,
                slot.x1,
                slot,
            );
        }
        Offline::Streamed => {}
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
    if let Actions::Reorder { playlist } = actions {
        cluster = cluster
            .push(Box::new(
                button("up", Icon::ChevronUp, "Move up", RowEvent::MoveUp(index))
                    .with_enabled(!matches!(row.place, Place::First | Place::Only)),
            ))
            .push(Box::new(
                button(
                    "down",
                    Icon::ChevronDown,
                    "Move down",
                    RowEvent::MoveDown(index),
                )
                .with_enabled(!matches!(row.place, Place::Last | Place::Only)),
            ))
            .push(Box::new(button(
                "remove",
                Icon::X,
                if playlist {
                    "Remove from playlist"
                } else {
                    "Remove from queue"
                },
                RowEvent::Remove(index),
            )));
    }
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
