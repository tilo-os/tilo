//! `Terminal` — a terminal's grid, drawn from the session a window hands in.
//!
//! The widget shows what a shell printed, a cell at a time, and sends back what the person did:
//! keys, pastes and pointer reports go to the session the window holds, and the one key table
//! that turns them into bytes is the host's. It keeps the presentation — the scrollback offset,
//! the selection, the bell's flash — and reports what the app records as its events: the title
//! a program set, the runs its shell marked, the shell's exit.
use std::sync::Arc;

use vello::kurbo::{Affine, Point, Rect, Vec2};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, Ime, MouseButton as WMouseButton, WindowEvent};
use winit::keyboard::{Key as WKey, NamedKey as WNamedKey};

use crate::color_compat::ColorCompat as _;
use crate::terminal::{
    CellColor, GridPos, GridSelection, Modes, Palette, Placement, Screen, Session, SessionEvent,
    SessionHandle, Style,
};
use crate::widget::OverlayScrollbar;
use crate::widget::scrollbar::PressOutcome;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::TextRole;
use tilo_ui_base::widget::ScrollMetrics;
use tilo_ui_base::{key_input, key_input::Button, key_input::Held, key_input::Keys};
use tilo_ui_base::{text_render::TextRenderer, text_render::TextStyle, text_render::TextWeight};
use tilo_ui_base::{widget::TaffyTreeExt, widget::Widget};

/// The height a grid takes where nothing bounds it, as on a page that scrolls: a screenful of
/// rows at the body text's size.
const UNBOUNDED_HEIGHT: f32 = 24.0 * 20.0;

#[path = "terminal/terminal_component.rs"]
mod component;
pub use component::TerminalProps;

/// How long the bell's flash stays on screen.
const BELL_FLASH: crate::time::Duration = crate::time::Duration::from_millis(140);

/// What a grid reports to the app that shows it, each as an event of the component.
#[derive(Default)]
pub struct GridEvents {
    /// The program set the window's title.
    pub title: tilo_ui_base::component::Event,
    /// A run the shell marked ended, as JSON.
    pub finished: tilo_ui_base::component::Event,
    /// The shell exited: "exited", or its status.
    pub exit: tilo_ui_base::component::Event,
    /// The connection to the shell was lost: why.
    pub lost: tilo_ui_base::component::Event,
    /// A selection was finished: the text it copied.
    pub selected: tilo_ui_base::component::Event,
    /// The bell rang.
    pub rung: tilo_ui_base::component::Event,
    /// The shell asked for the lines its Up recalls, as JSON.
    pub history: tilo_ui_base::component::Event,
    /// The session's host holds a command until someone acts, as JSON; empty once it let it go.
    pub held: tilo_ui_base::component::Event,
}

/// A terminal's grid: what a session shows and the input that reaches it.
pub struct Terminal {
    session: SessionHandle,
    /// Where the keys and the pointer go: the session's own destination through the window's
    /// table, so a remote desktop's view uses the same path.
    keys: Arc<dyn Keys>,
    session_id: String,
    rect: Rect,
    /// The grid inside the widget, inset so a glyph's overhang is not clipped.
    grid: Rect,
    cell: Option<TextStyle>,
    char_w: f64,
    char_h: f64,
    screen: Arc<Screen>,
    modes: Modes,
    revision: u64,
    /// The revision `screen` was taken at; `None` until the first frame fetched it.
    shown_revision: Option<u64>,
    painted_revision: u64,
    /// Rows scrolled up from the live bottom; 0 shows the newest output.
    scroll_rows: usize,
    /// The pixel part of a scroll between two rows, as the wheel and the glide moved it.
    sub: f64,
    glide: tilo_ui_base::ScrollMomentum,
    scrollbar: OverlayScrollbar,
    /// The first row the last frame drew, and how many it drew: where a pointer lands.
    first_row: usize,
    viewport_rows: usize,
    selection: Option<GridSelection>,
    selecting: bool,
    /// Where the last press was, how many times in a row, and when: a double or triple press
    /// selects the word or the line.
    presses: u8,
    pressed_at: Option<crate::time::Instant>,
    pointer: Point,
    placement_images: Option<tilo_ui_base::image_store::ImageStore>,
    placements: Vec<Placement>,
    focused: bool,
    test_id: String,
    accessible_name: String,
    copy_on_select: bool,
    bell_flash: bool,
    allow_clipboard: bool,
    bell_at: Option<crate::time::Instant>,
    reduce_motion: bool,
    event_cursor: u64,
    events: GridEvents,
    /// What a find in the scrollback found, by the rows the matches stand on, and which one is
    /// current.
    found: Vec<crate::widget::FindMatch>,
    /// Where each found match stands, as the row and the cells it covers.
    found_spans: Vec<(usize, usize, usize)>,
    found_at: usize,
    dirty: bool,
}

impl Terminal {
    /// A terminal over `session`.
    #[must_use]
    pub fn new(session: SessionHandle) -> Self {
        let keys: Arc<dyn Keys> = session.clone();
        Self {
            session,
            keys,
            session_id: String::new(),
            rect: Rect::ZERO,
            grid: Rect::ZERO,
            cell: None,
            char_w: 8.0,
            char_h: 18.0,
            screen: Arc::new(Screen::default()),
            modes: Modes::default(),
            revision: 0,
            shown_revision: None,
            painted_revision: 0,
            scroll_rows: 0,
            sub: 0.0,
            glide: tilo_ui_base::ScrollMomentum::new(),
            scrollbar: OverlayScrollbar::vertical(),
            first_row: 0,
            viewport_rows: 0,
            selection: None,
            selecting: false,
            presses: 0,
            pressed_at: None,
            pointer: Point::ZERO,
            placement_images: None,
            placements: Vec::new(),
            focused: false,
            test_id: "terminal".to_owned(),
            accessible_name: "Terminal".to_owned(),
            copy_on_select: false,
            bell_flash: false,
            allow_clipboard: false,
            bell_at: None,
            reduce_motion: false,
            event_cursor: 0,
            events: GridEvents::default(),
            found: Vec::new(),
            found_spans: Vec::new(),
            found_at: 0,
            dirty: true,
        }
    }

    #[must_use]
    pub fn with_copy_on_select(mut self, copy: bool) -> Self {
        self.copy_on_select = copy;
        self
    }

    /// Flash the grid when the bell rings, as the person's preference asks.
    #[must_use]
    pub fn with_bell_flash(mut self, flash: bool) -> Self {
        self.bell_flash = flash;
        self
    }

    /// Let the program put text on the clipboard (OSC 52), as the person's preference allows.
    #[must_use]
    pub fn with_allow_clipboard(mut self, allow: bool) -> Self {
        self.set_allow_clipboard(allow);
        self
    }

    /// Whether the program may put text on the clipboard now: the session follows at once.
    pub fn set_allow_clipboard(&mut self, allow: bool) {
        self.allow_clipboard = allow;
        self.session.allow_clipboard(allow);
    }

    #[must_use]
    pub fn with_accessible_name(mut self, name: impl Into<String>) -> Self {
        self.accessible_name = name.into();
        self
    }

    pub fn set_accessible_name(&mut self, name: impl Into<String>) {
        self.accessible_name = name.into();
    }

    #[must_use]
    pub fn with_test_id(mut self, id: &str) -> Self {
        id.clone_into(&mut self.test_id);
        self
    }

    /// The session this grid shows.
    #[must_use]
    pub fn session(&self) -> &SessionHandle {
        &self.session
    }

    /// Where this grid's keys and pointer go.
    pub fn set_keys(&mut self, keys: Arc<dyn Keys>) {
        self.keys = keys;
    }

    /// Show the session `id` names, as the window drawing now holds it.
    pub fn show_session(&mut self, id: &str) {
        if self.session_id == id {
            return;
        }
        id.clone_into(&mut self.session_id);
        self.session = crate::terminal::window_session(id)
            .unwrap_or_else(|| Arc::new(Missing) as SessionHandle);
        self.session.allow_clipboard(self.allow_clipboard);
        self.keys =
            key_input::window_input(id).unwrap_or_else(|| self.session.clone() as Arc<dyn Keys>);
        self.revision = 0;
        self.shown_revision = None;
        self.painted_revision = u64::MAX;
        self.scroll_rows = 0;
        self.sub = 0.0;
        self.selection = None;
        self.found.clear();
        self.found_spans.clear();
        self.event_cursor = 0;
        // The pictures were loaded through the session this grid showed; a new one reads its
        // own.
        self.placement_images = None;
        self.platform_resized();
        self.dirty = true;
    }

    /// Take the events a host attached to this grid, and report through them from now on.
    pub fn update_events(&mut self, events: GridEvents) {
        self.events = events;
    }

    /// Send the grid's size again, as a fresh session needs to hear it.
    pub fn platform_resized(&mut self) {
        self.viewport_rows = 0;
        let columns = self.columns_fit();
        let rows = self.rows_fit();
        self.viewport_rows = rows;
        self.session.resize(
            u16::try_from(columns).unwrap_or(u16::MAX),
            u16::try_from(rows).unwrap_or(u16::MAX),
        );
    }

    /// The grid this widget last drew, for whoever checks what a person sees.
    #[must_use]
    pub fn screen(&self) -> &Screen {
        &self.screen
    }

    #[must_use]
    pub fn selection(&self) -> Option<GridSelection> {
        self.selection
    }

    /// The text a selection would copy now.
    #[must_use]
    pub fn selected_text(&self) -> Option<String> {
        let selection = self.selection?;
        if selection.is_collapsed() {
            return None;
        }
        self.session
            .selection_text(selection.anchor, selection.cursor)
    }

    fn refresh(&mut self) {
        let revision = self.session.revision();
        if self.shown_revision == Some(revision) {
            return;
        }
        self.shown_revision = Some(revision);
        self.revision = revision;
        self.modes = self.session.modes();
        self.screen = self.session.screen();
        self.placements = self.session.placements();
        // Fresh output comes back to the live bottom, as a keystroke does: a person reading
        // scrollback sees what arrives instead of missing it.
        self.scroll_rows = 0;
        self.sub = 0.0;
        self.glide.stop();
        self.dirty = true;
    }

    /// Poll the session's reports and hand them to the app's events.
    fn report(&mut self) {
        let (events, cursor) = self.session.events(self.event_cursor);
        if events.is_empty() {
            self.event_cursor = cursor;
            return;
        }
        self.event_cursor = cursor;
        for event in events {
            match event {
                SessionEvent::Title(title) => self.fired(&self.events.title, title),
                SessionEvent::Finished(json) => self.fired(&self.events.finished, json),
                SessionEvent::Exit(code) => {
                    self.fired(
                        &self.events.exit,
                        if code == 0 {
                            "exited".to_owned()
                        } else {
                            format!("exit {code}")
                        },
                    );
                    self.dirty = true;
                }
                SessionEvent::Lost(why) => {
                    self.fired(&self.events.lost, why);
                    self.dirty = true;
                }
                SessionEvent::Bell => {
                    if self.bell_flash {
                        self.bell_at = Some(crate::time::now());
                    }
                    self.fired(&self.events.rung, String::new());
                }
                SessionEvent::Clipboard(text, primary) => {
                    if primary {
                        tilo_ui_base::text_selection::clipboard_copy_primary(&text);
                    } else {
                        tilo_ui_base::text_selection::clipboard_copy(&text);
                    }
                }
                SessionEvent::History(ask) => self.fired(&self.events.history, ask),
                SessionEvent::Held(held) => {
                    self.fired(&self.events.held, held);
                    self.dirty = true;
                }
            }
        }
    }

    fn fired(&self, event: &tilo_ui_base::component::Event, value: String) {
        if let tilo_ui_base::component::Event::View(intent) = event {
            intent.fire(Some(tilo_ui_base::component::Value::Text(value)));
        }
    }

    fn bell_flashing(&self) -> bool {
        self.bell_at
            .is_some_and(|at| crate::time::now().saturating_duration_since(at) < BELL_FLASH)
    }

    fn rows_fit(&self) -> usize {
        if self.char_h <= 0.0 {
            return 1;
        }
        ((self.grid.height() / self.char_h).floor() as usize).max(1)
    }

    fn columns_fit(&self) -> usize {
        if self.char_w <= 0.0 {
            return 1;
        }
        ((self.grid.width() / self.char_w).floor() as usize).max(1)
    }

    /// The most rows the view can be scrolled up from the live bottom.
    fn max_scroll(&self) -> usize {
        self.screen.lines.len().saturating_sub(self.rows_fit())
    }

    fn scroll_to(&mut self, rows: usize) {
        if self.screen.alternate {
            return;
        }
        self.scroll_rows = rows.min(self.max_scroll());
        if self.scroll_rows == 0 {
            self.sub = 0.0;
        }
        self.glide.stop();
        self.dirty = true;
    }

    fn scroll_by(&mut self, rows: isize) {
        let to = if rows >= 0 {
            self.scroll_rows.saturating_add(rows.unsigned_abs())
        } else {
            self.scroll_rows.saturating_sub(rows.unsigned_abs())
        };
        self.scroll_to(to);
    }

    /// Move the scrollback by `px` logical pixels, positive showing older rows.
    fn scroll_px(&mut self, px: f64) -> f64 {
        if self.screen.alternate || !px.is_finite() || self.char_h <= 0.0 {
            return 0.0;
        }
        let top = self.scroll_rows as f64 * self.char_h + self.sub;
        let max = self.max_scroll() as f64 * self.char_h;
        let to = (top + px).clamp(0.0, max);
        let rows = (to / self.char_h).floor();
        self.scroll_rows = (rows as usize).min(self.max_scroll());
        self.sub = if self.scroll_rows == 0 {
            0.0
        } else {
            to - rows * self.char_h
        };
        self.dirty = true;
        to - top
    }

    /// The buffer row a pointer position lands on, and its column.
    fn cell_at(&self, at: Point) -> GridPos {
        let x = ((at.x - self.grid.x0).max(0.0) / self.char_w.max(1.0)).floor() as usize;
        let y = ((at.y - self.grid.y0).max(0.0) + self.sub).floor() / self.char_h.max(1.0);
        let view_row = (y as usize).min(self.viewport_rows.saturating_sub(1));
        let row = (self.first_row + view_row).min(self.screen.lines.len().saturating_sub(1));
        GridPos::new(row, x.min(self.columns_fit().saturating_sub(1)))
    }

    fn line(&self, row: usize) -> &str {
        self.screen
            .lines
            .get(row)
            .map_or("", |line| line.text.as_str())
    }

    /// The character cell `col` of `row` holds, for a word press.
    fn char_at(&self, at: GridPos) -> Option<(usize, usize)> {
        let line = self.line(at.row);
        let mut index = 0;
        for (byte, _) in line.char_indices() {
            if index == at.col {
                return Some((byte, index));
            }
            index += 1;
        }
        (at.col == index).then_some((line.len(), index))
    }

    fn word_at(&self, at: GridPos) -> (GridPos, GridPos) {
        let line = self.line(at.row);
        let byte = self.char_at(at).map_or(0, |(byte, _)| byte);
        let (start, end) = tilo_ui_base::text_selection::word_boundaries(line, byte);
        let cell = |byte: usize| {
            line.get(..byte.min(line.len()))
                .map_or(0, |head| head.chars().count())
        };
        (
            GridPos::new(at.row, cell(start)),
            GridPos::new(
                at.row,
                cell(end).max(cell(start) + 1).min(self.columns_fit()),
            ),
        )
    }

    fn line_span(&self, at: GridPos) -> (GridPos, GridPos) {
        (
            GridPos::new(at.row, 0),
            GridPos::new(at.row, self.columns_fit()),
        )
    }

    fn begin_selection(&mut self, at: GridPos, extend: bool) {
        if extend {
            let anchor = self.selection.map_or(at, |held| held.anchor);
            self.selection = Some(GridSelection { anchor, cursor: at });
        } else {
            self.selection = Some(GridSelection {
                anchor: at,
                cursor: at,
            });
        }
        self.selecting = true;
        self.dirty = true;
    }

    fn finish_selection(&mut self) {
        self.selecting = false;
        let Some(selection) = self.selection else {
            return;
        };
        if selection.is_collapsed() {
            self.selection = None;
            self.dirty = true;
            return;
        }
        if let Some(text) = self.selected_text().filter(|text| !text.is_empty()) {
            tilo_ui_base::text_selection::clipboard_copy_primary(&text);
            if self.copy_on_select {
                tilo_ui_base::text_selection::clipboard_copy(&text);
            }
            self.fired(&self.events.selected, text);
        }
        self.dirty = true;
    }

    /// The link the cell at `at` names, when it is a public web address.
    fn link_at(&self, at: GridPos) -> Option<&str> {
        let style = self
            .screen
            .lines
            .get(at.row)
            .and_then(|line| line.styles.get(at.col));
        let link = style?.link;
        let url = self.screen.links.get(&link)?;
        self.session.public_link(url).then_some(url.as_str())
    }

    /// The cell a pointer column and row report, one-based.
    fn report_cell(at: GridPos) -> (usize, usize) {
        (at.col.saturating_add(1), at.row.saturating_add(1))
    }

    fn press(
        &mut self,
        button: WMouseButton,
        held: Held,
        ctx: &mut tilo_ui_base::widget::EventCtx<'_>,
    ) {
        let at = self.cell_at(self.pointer);
        let report = self.modes.mouse_report && !held.shift;
        match button {
            WMouseButton::Left => {
                *ctx.focus_target = Some(ctx.node_id);
                if report {
                    let (column, row) = Self::report_cell(at);
                    self.keys.mouse(Button::Left, true, held, column, row);
                    *ctx.handled = true;
                    return;
                }
                let now = crate::time::now();
                let quickly = self.pressed_at.is_some_and(|then| {
                    now.saturating_duration_since(then) < crate::time::Duration::from_millis(400)
                });
                self.presses = if quickly {
                    (self.presses + 1).min(3)
                } else {
                    1
                };
                self.pressed_at = Some(now);
                match self.presses {
                    2 => {
                        let (start, end) = self.word_at(at);
                        self.selection = Some(GridSelection {
                            anchor: start,
                            cursor: end,
                        });
                        self.selecting = true;
                    }
                    3 => {
                        let (start, end) = self.line_span(at);
                        self.selection = Some(GridSelection {
                            anchor: start,
                            cursor: end,
                        });
                        self.selecting = true;
                    }
                    _ => self.begin_selection(at, held.shift),
                }
                self.dirty = true;
                *ctx.handled = true;
            }
            WMouseButton::Middle if !report => {
                if let Some(text) = tilo_ui_base::text_selection::clipboard_paste_primary() {
                    self.keys.paste(&text);
                }
                *ctx.handled = true;
            }
            // A right press with the program not tracking the mouse is the app's menu: it
            // stays unhandled, so the area the view put it in opens it.
            _ => {
                if report {
                    let mapped = if button == WMouseButton::Right {
                        Button::Right
                    } else {
                        Button::Middle
                    };
                    let (column, row) = Self::report_cell(at);
                    self.keys.mouse(mapped, true, held, column, row);
                    *ctx.handled = true;
                }
            }
        }
    }

    fn release(
        &mut self,
        button: WMouseButton,
        held: Held,
        ctx: &mut tilo_ui_base::widget::EventCtx<'_>,
    ) {
        let report = self.modes.mouse_report && !held.shift;
        if report {
            let mapped = match button {
                WMouseButton::Left => Button::Left,
                WMouseButton::Middle => Button::Middle,
                WMouseButton::Right => Button::Right,
                _ => return,
            };
            let at = self.cell_at(self.pointer);
            let (column, row) = Self::report_cell(at);
            self.keys.mouse(mapped, false, held, column, row);
            *ctx.handled = true;
            return;
        }
        if button == WMouseButton::Left && self.selecting {
            if self
                .selection
                .is_some_and(|selection| selection.is_collapsed())
            {
                if let Some(url) = self
                    .link_at(self.selection.map_or(GridPos::default(), |s| s.cursor))
                    .map(str::to_owned)
                {
                    tilo_ui_base::platform::os_open(&url);
                }
            }
            self.finish_selection();
            *ctx.handled = true;
        }
        if button == WMouseButton::Left && self.scrollbar.release() {
            *ctx.handled = true;
        }
    }

    fn scrollbar_metrics(&self) -> ScrollMetrics {
        let total = self.screen.lines.len().max(self.viewport_rows);
        let viewport = self.viewport_rows.max(1);
        let from_bottom =
            self.scroll_rows.min(self.max_scroll()) as f64 + self.sub / self.char_h.max(1.0);
        let offset = (total as f64 - viewport as f64 - from_bottom).max(0.0);
        ScrollMetrics::new(total as f64, viewport as f64, offset)
    }

    fn palette(theme: &Theme) -> Palette {
        Palette::from_theme(theme)
    }

    fn draw_placements(
        &mut self,
        scene: &mut vello::Scene,
        transform: Affine,
        theme: &Theme,
        first: usize,
    ) {
        let placements = self.placements.clone();
        if placements.is_empty() {
            return;
        }
        let Some(store) = self.placement_images.as_mut() else {
            return;
        };
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            transform,
            &self.grid,
        );
        for placement in &placements {
            if placement.alternate != self.screen.alternate {
                continue;
            }
            let columns = usize::try_from(placement.columns.clamp(1, 1024)).unwrap_or(1);
            let rows = usize::try_from(placement.rows.clamp(1, 1024)).unwrap_or(1);
            if placement.row < first || placement.row >= first + self.viewport_rows {
                continue;
            }
            let left = self.grid.x0 + placement.column as f64 * self.char_w;
            let top = self.grid.y0 + (placement.row - first) as f64 * self.char_h - self.sub;
            let right = (left + columns as f64 * self.char_w).min(self.grid.x1);
            let bottom = (top + rows as f64 * self.char_h).min(self.grid.y1);
            let rect = Rect::new(left, top, right, bottom);
            if rect.width() <= 0.0 || rect.height() <= 0.0 {
                continue;
            }
            let url = format!("tilo-blob://{}", placement.hash);
            if let Some(image) = store.get(&url) {
                tilo_ui_base::image_store::paint_image_contain(scene, transform, rect, &image, 0.0);
            } else {
                scene.stroke(
                    &vello::kurbo::Stroke::new(1.0),
                    transform,
                    theme.text_tertiary,
                    None,
                    &rect,
                );
            }
        }
        scene.pop_layer();
    }

    fn draw_row(&self, pen: &mut Pen<'_>, theme: &Theme, row: usize, y: f64) {
        let Some(line) = self.screen.lines.get(row) else {
            return;
        };
        let (scene, transform) = (&mut *pen.scene, pen.transform);
        if let Some(selection) = self.selection
            && let Some((from, to)) = selection.span_on(row, self.columns_fit())
        {
            let x0 = self.grid.x0 + from as f64 * self.char_w;
            let x1 = self.grid.x0 + to as f64 * self.char_w;
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.intent.selection_tint,
                None,
                &Rect::new(x0, y, x1, y + self.char_h),
            );
        }
        for (index, (found_row, from, to)) in self.found_spans.iter().enumerate() {
            if *found_row != row {
                continue;
            }
            let x0 = self.grid.x0 + *from as f64 * self.char_w;
            let x1 = (self.grid.x0 + *to as f64 * self.char_w).max(x0 + self.char_w);
            crate::widget::paint_find_match(
                scene,
                transform,
                Rect::new(x0, y, x1, y + self.char_h),
                index == self.found_at,
                theme,
            );
        }
        let style_of = |cell: usize| line.styles.get(cell).copied().unwrap_or_default();
        let mut run = String::new();
        let mut run_style: Option<Style> = None;
        let mut run_start = 0usize;
        for (cell, character) in line.text.chars().enumerate() {
            if cell >= self.columns_fit() {
                break;
            }
            let style = style_of(cell);
            if run_style != Some(style) {
                if let Some(drawn) = run_style {
                    self.draw_run(pen, &run, drawn, run_start, y);
                }
                run.clear();
                run_start = cell;
                run_style = Some(style);
            }
            run.push(character);
        }
        if let Some(drawn) = run_style {
            self.draw_run(pen, &run, drawn, run_start, y);
        }
    }

    fn draw_run(&self, pen: &mut Pen<'_>, run: &str, style: Style, start_cell: usize, y: f64) {
        let Pen {
            scene,
            transform,
            text,
            palette,
        } = pen;
        let (transform, palette) = (*transform, &**palette);
        let cells = run.chars().count();
        let x = self.grid.x0 + start_cell as f64 * self.char_w;
        let width = cells as f64 * self.char_w;
        let foreground = crate::terminal::text_color(style.fg, style.bold, palette);
        let background = crate::terminal::background_color(style.bg, palette);
        let (fg, bg) = if style.reverse {
            (background, foreground)
        } else {
            (foreground, background)
        };
        let fg = if style.dim {
            fg.with_alpha_factor(0.55)
        } else {
            fg
        };
        let fg = if style.link != 0 && style.fg == CellColor::Default && !style.reverse {
            palette.ansi_256(12)
        } else {
            fg
        };
        if bg != palette.background {
            scene.fill(
                Fill::NonZero,
                transform,
                bg,
                None,
                &Rect::new(x, y, x + width, y + self.char_h),
            );
        }
        let Some(cell) = self.cell.clone() else {
            return;
        };
        let weight = if style.bold {
            TextWeight::Bold
        } else {
            TextWeight::Regular
        };
        text.draw_style(
            scene,
            run,
            fg,
            transform * Affine::translate(Vec2::new(x, y)),
            None,
            &cell.clone().with_weight(weight).with_italic(style.italic),
        );
        if style.underline || style.link != 0 {
            let uy = y + self.char_h - 1.5;
            scene.fill(
                Fill::NonZero,
                transform,
                fg,
                None,
                &Rect::new(x, uy, x + width, uy + 1.0),
            );
        }
        if style.strike {
            let sy = y + (self.char_h * 0.5).floor();
            scene.fill(
                Fill::NonZero,
                transform,
                fg,
                None,
                &Rect::new(x, sy, x + width, sy + 1.0),
            );
        }
    }

    fn draw_cursor(&self, pen: &mut Pen<'_>, first: usize) {
        let Pen {
            scene,
            transform,
            text,
            palette,
        } = pen;
        let transform = *transform;
        if !self.focused || !self.modes.cursor_visible {
            return;
        }
        let (column, row) = self.screen.cursor;
        if row < first || row >= first + self.viewport_rows || column >= self.columns_fit() {
            return;
        }
        let x = self.grid.x0 + column as f64 * self.char_w;
        let y = self.grid.y0 + (row - first) as f64 * self.char_h - self.sub;
        let block = Rect::new(x, y, x + self.char_w, y + self.char_h);
        let ring = Rect::new(
            x + 0.5,
            y + 0.5,
            x + self.char_w - 0.5,
            y + self.char_h - 0.5,
        );
        let character = self
            .screen
            .lines
            .get(row)
            .and_then(|line| line.text.chars().nth(column));
        if let Some(character) = character.filter(|c| !c.is_whitespace()) {
            // An inverted cell: the block is drawn and the glyph again in the ground colour.
            scene.fill(Fill::NonZero, transform, palette.foreground, None, &block);
            if let Some(cell) = self.cell.clone() {
                text.draw_style(
                    scene,
                    &character.to_string(),
                    palette.background,
                    transform * Affine::translate(Vec2::new(x, y)),
                    None,
                    &cell,
                );
            }
        } else {
            scene.fill(Fill::NonZero, transform, palette.foreground, None, &ring);
        }
    }
}

/// What a row of the grid is drawn with: the scene and frame, the text renderer, and the colours
/// the terminal's own palette gives its cells.
struct Pen<'a> {
    scene: &'a mut vello::Scene,
    transform: Affine,
    text: &'a mut TextRenderer,
    palette: &'a Palette,
}

impl Widget for Terminal {
    tilo_ui_base::described!();

    fn name(&self) -> &'static str {
        "Terminal"
    }

    fn test_id(&self) -> Option<&str> {
        Some(&self.test_id)
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        // It takes the room its pane gives it, beside a banner or a bar in the same column, and a
        // screenful of rows where nothing bounds it (a scrolling page).
        let style = taffy::Style {
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::auto(),
            },
            min_size: taffy::Size {
                width: taffy::Dimension::auto(),
                height: taffy::Dimension::length(0.0),
            },
            flex_grow: 1.0,
            flex_shrink: 1.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        _text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let definite = |space: taffy::AvailableSpace| match space {
            taffy::AvailableSpace::Definite(length) => Some(length),
            taffy::AvailableSpace::MinContent | taffy::AvailableSpace::MaxContent => None,
        };
        taffy::Size {
            width: known.width.or(definite(available.width)).unwrap_or(0.0),
            height: known
                .height
                .or(definite(available.height))
                .unwrap_or(UNBOUNDED_HEIGHT),
        }
    }

    fn apply_layout(&mut self, taffy: &taffy::TaffyTree<()>, node: taffy::NodeId, origin: Point) {
        let rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
        if rect != self.rect {
            self.rect = rect;
            self.dirty = true;
        }
    }

    /// It fills the box it is given and fits its cells to it as it paints them, so it asks layout
    /// only until it is placed.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }

    fn sync(
        &mut self,
        _state: &tilo_ui_base::widget::AppModel,
        _path: &tilo_ui_base::widget::ModelPath,
    ) {
        self.report();
        self.refresh();
        if self.bell_flashing() {
            self.dirty = true;
        }
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(unused_variables, reason = "a draw binds only the parts it uses")]
        let (scene, transform, text_renderer, theme, _env) = ctx.parts();
        if self.rect.width() <= 0.0 || self.rect.height() <= 0.0 {
            return;
        }
        self.report();
        self.refresh();
        self.reduce_motion = theme.reduce_motion;
        let palette = Self::palette(theme);
        let cell = theme.text(TextRole::Code);
        let (w, h) = text_renderer.measure_style(" ", None, &cell);
        self.char_w = f64::from(w).max(1.0);
        self.char_h = f64::from(h).ceil().max(1.0);
        self.cell = Some(cell);
        self.grid = self.rect;
        let columns = self.columns_fit();
        let rows = self.rows_fit();
        if self.viewport_rows != rows {
            self.viewport_rows = rows;
            self.session.resize(
                u16::try_from(columns).unwrap_or(u16::MAX),
                u16::try_from(rows).unwrap_or(u16::MAX),
            );
        }
        scene.fill(
            Fill::NonZero,
            transform,
            palette.background,
            None,
            &self.rect,
        );
        let first = if self.screen.alternate {
            0
        } else {
            self.screen
                .lines
                .len()
                .saturating_sub(rows)
                .saturating_sub(self.scroll_rows.min(self.max_scroll()))
        };
        self.first_row = first;
        let mut pen = Pen {
            scene,
            transform,
            text: text_renderer,
            palette: &palette,
        };
        for index in 0..rows {
            let row = first + index;
            if row >= self.screen.lines.len() {
                break;
            }
            let y = self.grid.y0 + index as f64 * self.char_h - self.sub;
            self.draw_row(&mut pen, theme, row, y);
        }
        self.draw_cursor(&mut pen, first);
        self.images();
        self.draw_placements(scene, transform, theme, first);
        self.scrollbar.set_theme(theme);
        let metrics = self.scrollbar_metrics();
        self.scrollbar.note_offset(metrics.offset);
        self.scrollbar
            .paint(scene, transform, self.rect, metrics, theme);
        if self.bell_flashing() {
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.base.with_alpha_factor(0.18),
                None,
                &self.rect,
            );
        }
        self.painted_revision = self.revision;
        self.dirty = false;
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut tilo_ui_base::widget::EventCtx<'_>) {
        let held = key_input::held(&ctx.modifiers.state());
        match event {
            WindowEvent::Ime(Ime::Commit(text)) if self.focused && !text.is_empty() => {
                self.keys.text(text);
                self.dirty = true;
                *ctx.handled = true;
            }
            WindowEvent::Focused(focused) if self.modes.focus_report => {
                self.keys.focus(*focused);
                *ctx.handled = true;
            }
            WindowEvent::KeyboardInput { event: key, .. } => {
                if !self.focused || key.state != ElementState::Pressed {
                    return;
                }
                let modifiers = ctx.modifiers.state();
                // The terminal's own chords; they come before Ctrl+C, which is the shell's.
                if modifiers.control_key()
                    && modifiers.shift_key()
                    && let WKey::Character(c) = &key.logical_key
                {
                    if c.eq_ignore_ascii_case("c") {
                        if let Some(text) = self.selected_text() {
                            tilo_ui_base::text_selection::clipboard_copy(&text);
                        }
                        *ctx.handled = true;
                        return;
                    }
                    if c.eq_ignore_ascii_case("v") {
                        if let Some(text) = tilo_ui_base::text_selection::clipboard_paste() {
                            self.keys.paste(&text);
                            self.scroll_to(0);
                        }
                        *ctx.handled = true;
                        return;
                    }
                }
                if modifiers.shift_key()
                    && matches!(key.logical_key, WKey::Named(WNamedKey::Insert))
                {
                    if let Some(text) = tilo_ui_base::text_selection::clipboard_paste() {
                        self.keys.paste(&text);
                        self.scroll_to(0);
                    }
                    *ctx.handled = true;
                    return;
                }
                // Shift+Page Up and Shift+Page Down move the scrollback; without Shift they are
                // the program's.
                if modifiers.shift_key()
                    && !self.screen.alternate
                    && let WKey::Named(named) = &key.logical_key
                {
                    let page = self.viewport_rows.saturating_sub(1).max(1);
                    match named {
                        WNamedKey::PageUp => {
                            self.scroll_by(page.cast_signed());
                            *ctx.handled = true;
                            return;
                        }
                        WNamedKey::PageDown => {
                            self.scroll_by(-page.cast_signed());
                            *ctx.handled = true;
                            return;
                        }
                        WNamedKey::Home => {
                            self.scroll_to(self.max_scroll());
                            *ctx.handled = true;
                            return;
                        }
                        WNamedKey::End => {
                            self.scroll_to(0);
                            *ctx.handled = true;
                            return;
                        }
                        _ => {}
                    }
                }
                // With the command modifier alone, the window's shortcuts are the window's.
                if modifiers.super_key() && !modifiers.control_key() && !modifiers.alt_key() {
                    return;
                }
                // Ctrl+Plus, Minus and 0 zoom the window, never bytes for the shell; Ctrl+Tab and
                // Ctrl+Shift+T are the desktop's tab chords, with no bytes of their own. Ctrl+T
                // and Ctrl+W stay the shell's.
                let conventions = tilo_ui_base::platform::PlatformConventions::default();
                if crate::content_zoom::ZoomStep::from_key(conventions, modifiers, &key.logical_key)
                    .is_some()
                    || matches!(
                        crate::shell_tabs::chord_request(
                            &key.logical_key,
                            conventions.command_modifier_down(modifiers),
                            modifiers.shift_key(),
                            modifiers.alt_key()
                                || (modifiers.control_key() && modifiers.super_key()),
                        ),
                        Some(
                            crate::shell_tabs::TabRequest::CycleTab { .. }
                                | crate::shell_tabs::TabRequest::ReopenClosed
                        )
                    )
                {
                    return;
                }
                if let Some(mapped) = key_input::map_key(&key.logical_key) {
                    let text = key.text.as_deref();
                    self.keys.key(mapped, text, held);
                    self.scroll_to(0);
                    self.dirty = true;
                    *ctx.handled = true;
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let at = ctx.logical(*position);
                self.pointer = at;
                if self.selecting {
                    let cell = self.cell_at(at);
                    if let Some(selection) = self.selection.as_mut() {
                        selection.cursor = cell;
                    }
                    self.dirty = true;
                    *ctx.handled = true;
                } else if self
                    .scrollbar
                    .update_pointer(self.rect, self.scrollbar_metrics(), at)
                {
                    self.dirty = true;
                }
            }
            WindowEvent::CursorLeft { .. } => {
                self.scrollbar.pointer_left();
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if !self.grid.contains(self.pointer) && !self.selecting {
                    return;
                }
                match state {
                    ElementState::Pressed => {
                        if self.grid.contains(self.pointer) {
                            let metrics = self.scrollbar_metrics();
                            match self.scrollbar.press(self.rect, metrics, self.pointer) {
                                PressOutcome::Miss => self.press(*button, held, ctx),
                                PressOutcome::Page(offset) => {
                                    self.scroll_to(
                                        self.max_scroll().saturating_sub(offset.round() as usize),
                                    );
                                    *ctx.handled = true;
                                }
                                PressOutcome::Thumb => *ctx.handled = true,
                            }
                        }
                    }
                    ElementState::Released => self.release(*button, held, ctx),
                }
            }
            WindowEvent::MouseWheel { delta, phase, .. } => {
                if crate::content_zoom::is_zoom_scroll(event, ctx.modifiers) {
                    return;
                }
                let step = self.glide.wheel(delta, *phase, self.reduce_motion);
                match step {
                    tilo_ui_base::WheelStep::Scroll(px) => {
                        self.scroll_px(-px);
                        *ctx.handled = true;
                    }
                    tilo_ui_base::WheelStep::Fling => *ctx.handled = true,
                    tilo_ui_base::WheelStep::Ignored => {}
                }
            }
            _ => {}
        }
    }

    fn next_wake_deadline(&self) -> Option<crate::time::Instant> {
        self.bell_at
            .map(|at| at + BELL_FLASH)
            .into_iter()
            .chain(self.scrollbar.next_wake_deadline())
            .min()
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
            || self.painted_revision != self.session.revision()
            || self.bell_flashing()
            || self.glide.is_active()
            || self.scrollbar.needs_animation_frame()
    }

    fn needs_animation_frame(&self) -> bool {
        self.bell_flashing() || self.glide.is_active() || self.scrollbar.needs_animation_frame()
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        if self.focused != focused {
            self.focused = focused;
            self.keys.focus(focused);
            self.dirty = true;
        }
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    /// Tab is the shell's: a program completes its own words with it.
    fn wants_tab_input(&self) -> bool {
        true
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.rect
            .contains(point)
            .then_some(winit::window::CursorIcon::Text)
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        self.focused.then(|| {
            let (column, row) = self.screen.cursor;
            let x = self.grid.x0 + column as f64 * self.char_w;
            let y =
                self.grid.y0 + (row.saturating_sub(self.first_row)) as f64 * self.char_h - self.sub;
            Rect::new(x, y, x + self.char_w, y + self.char_h)
        })
    }

    fn ime_content(&self) -> Option<tilo_ui_base::ime::InputContent> {
        (self.focused && !self.session.ended()).then_some(tilo_ui_base::ime::InputContent::TERMINAL)
    }

    fn commit_deferred_text(&mut self, text: &str) -> Option<crate::text_input::Receipt> {
        // A shell has no fields to receipt: its input is the pty, and the text is written there as
        // an input method's commit is.
        if self.focused && !self.session.ended() {
            self.session.text(text);
            self.dirty = true;
        }
        None
    }

    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        use tilo_ui_base::{a11y::AccessNode, a11y::AccessState, a11y::Role};
        Some(
            AccessNode::new(Role::Terminal)
                .with_name(self.accessible_name.clone())
                .with_description(
                    self.screen
                        .lines
                        .last()
                        .map_or_else(String::new, |line| line.text.trim_end().to_owned()),
                )
                .with_hashed_key("terminal")
                .with_state(AccessState::new().focusable(true).focused(self.focused)),
        )
    }
}

impl crate::widget::Findable for Terminal {
    fn find(
        &mut self,
        query: &str,
        options: crate::widget::FindOptions,
    ) -> Vec<crate::widget::FindMatch> {
        self.found.clear();
        self.found_spans.clear();
        self.found_at = 0;
        if !query.is_empty() {
            for (row, line) in self.screen.lines.iter().enumerate() {
                let haystack = if options.match_case {
                    line.text.clone()
                } else {
                    line.text.to_lowercase()
                };
                let needle = if options.match_case {
                    query.to_owned()
                } else {
                    query.to_lowercase()
                };
                let mut from = 0;
                while let Some(at) = haystack.get(from..).and_then(|rest| rest.find(&needle)) {
                    let start = from + at;
                    let end = start + needle.len();
                    let whole = options.whole_word;
                    let before = haystack
                        .get(..start)
                        .and_then(|head| head.chars().next_back());
                    let after = haystack.get(end..).and_then(|tail| tail.chars().next());
                    let bounded =
                        |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric() && c != '_');
                    if !whole || (bounded(before) && bounded(after)) {
                        // The bar counts matches; the grid paints each where its cells stand.
                        self.found.push(crate::widget::FindMatch::new(row, end));
                        let cell = |byte: usize| {
                            line.text
                                .get(..byte.min(line.text.len()))
                                .map_or(0, |head| head.chars().count())
                        };
                        self.found_spans
                            .push((row, cell(start), cell(end).max(cell(start) + 1)));
                    }
                    from = end.max(start + 1);
                    if from >= haystack.len() {
                        break;
                    }
                }
            }
        }
        self.dirty = true;
        self.found.clone()
    }

    fn reveal(&mut self, index: usize) {
        let Some(found) = self.found.get(index).cloned() else {
            return;
        };
        self.found_at = index;
        let row = found.start;
        let top = self.first_row;
        if row >= top && row < top + self.viewport_rows {
            self.dirty = true;
            return;
        }
        let rows = self.viewport_rows.max(1);
        let want_top = row.saturating_sub(rows / 2);
        let max_top = self.screen.lines.len().saturating_sub(rows);
        let from_bottom = max_top.saturating_sub(want_top.min(max_top));
        self.scroll_to(from_bottom);
        self.dirty = true;
    }

    fn mark_current(&mut self, index: usize) {
        if index < self.found.len() {
            self.found_at = index;
        }
        self.dirty = true;
    }

    fn content_revision(&self) -> u64 {
        self.session.revision()
    }
}

/// A session that is not there: a view naming one the window does not hold draws an empty grid.
pub struct Missing;

impl Keys for Missing {
    fn key(&self, _key: key_input::Key, _text: Option<&str>, _held: Held) {}

    fn text(&self, _text: &str) {}

    fn paste(&self, _text: &str) {}

    fn mouse(
        &self,
        _button: Button,
        _pressed: bool,
        _held: Held,
        _column: usize,
        _row: usize,
    ) -> bool {
        false
    }

    fn focus(&self, _focused: bool) {}
}

impl Session for Missing {
    fn screen(&self) -> Arc<Screen> {
        Arc::new(Screen::default())
    }

    fn modes(&self) -> Modes {
        Modes::default()
    }

    fn revision(&self) -> u64 {
        0
    }

    fn resize(&self, _columns: u16, _rows: u16) {}

    fn selection_text(&self, _anchor: GridPos, _cursor: GridPos) -> Option<String> {
        None
    }

    fn placements(&self) -> Vec<Placement> {
        Vec::new()
    }

    fn image_bytes(&self, _hash: &str) -> Option<Vec<u8>> {
        None
    }

    fn public_link(&self, _url: &str) -> bool {
        false
    }

    fn events(&self, since: u64) -> (Vec<SessionEvent>, u64) {
        (Vec::new(), since)
    }

    fn ended(&self) -> bool {
        true
    }

    fn allow_clipboard(&self, _allow: bool) {}
}

impl Terminal {
    /// Build the image store the placements' pictures load through, once the session is known.
    fn images(&mut self) -> &mut tilo_ui_base::image_store::ImageStore {
        let session = Arc::clone(&self.session);
        self.placement_images.get_or_insert_with(|| {
            let fetch: tilo_ui_base::image_store::Fetcher =
                Arc::new(move |url: &str| session.image_bytes(url.strip_prefix("tilo-blob://")?));
            tilo_ui_base::image_store::ImageStore::new(&fetch)
        })
    }
}

#[cfg(test)]
#[path = "terminal_tests.rs"]
mod terminal_tests;
