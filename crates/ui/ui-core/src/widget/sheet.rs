//! A spreadsheet's grid as one widget: column and row headers, the cells a window shows, the
//! selection, an in-cell editor, column and row resizing and the fill handle.
//!
//! The app owns the data: it hands the grid the cells it shows and the selection it keeps, and
//! the grid reports the changes a person makes as events. Only the rows and columns on screen are
//! laid out and painted, so a sheet of a hundred thousand cells costs its window.

use crate::a11y::{AccessNode, AccessState, Role};
use std::collections::{HashMap, HashSet};

use taffy::TaffyTree;
use vello::kurbo::{Line, Point, Rect, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, Ime, KeyEvent, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::paint::PaintCtx;
use crate::surface::{Region, edge_on};
use crate::text_edit::{EditCore, Motion};
use crate::text_render::{TextRenderer, TextStyle, TextWeight};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::find_bar::{FindMatch, FindOptions, Findable};
use crate::widget::scrollbar::{ScrollMetrics, Scrollbar};
use crate::widget::{EventCtx, NodeId, TaffyTreeExt, Widget};

/// A zero-based cell: its column, then its row.
pub type Addr = (u32, u32);

/// How a cell's text reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CellKind {
    #[default]
    Text,
    Number,
    Bool,
    Error,
}

/// One cell as the app hands it to the grid.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SheetCell {
    pub cell: String,
    pub text: String,
    pub kind: CellKind,
    pub bold: bool,
}

impl SheetCell {
    /// Its address, when its reference is one.
    fn addr(&self) -> Option<Addr> {
        parse_ref(&self.cell)
    }
}

/// Another person at work in the sheet: the cells they chose and whose they are.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Peer {
    pub area: String,
    pub who: String,
}

/// A column's width or a row's height, where the person sized it.
#[derive(Clone, Debug, PartialEq)]
pub enum SheetSize {
    Column { column: String, width: f64 },
    Row { row: u32, height: f64 },
}

/// A sheet's letters for a zero-based column.
#[must_use]
pub fn col_letters(column: u32) -> String {
    let mut out = String::new();
    let mut n = column.saturating_add(1);
    while n > 0 {
        let digit = u32::try_from((n - 1) % 26).unwrap_or(0);
        let letter = char::from_u32(u32::from(b'A') + digit).unwrap_or('A');
        out.insert(0, letter);
        n = (n - 1) / 26;
    }
    out
}

/// A zero-based column for `letters`, or `None` when they name no column.
#[must_use]
pub fn col_index(letters: &str) -> Option<u32> {
    if letters.is_empty() || letters.len() > 3 {
        return None;
    }
    let mut n: u32 = 0;
    for ch in letters.chars() {
        if !ch.is_ascii_alphabetic() {
            return None;
        }
        let digit = u32::from(ch.to_ascii_uppercase() as u8 - b'A');
        n = n * 26 + digit + 1;
    }
    n.checked_sub(1)
}

/// A cell's A1 reference.
#[must_use]
pub fn cell_ref(addr: Addr) -> String {
    format!("{}{}", col_letters(addr.0), addr.1.saturating_add(1))
}

/// The zero-based address a reference names.
#[must_use]
pub fn parse_ref(text: &str) -> Option<Addr> {
    let text = text.trim();
    let letters: String = text
        .chars()
        .skip_while(|ch| *ch == '$')
        .take_while(char::is_ascii_alphabetic)
        .collect();
    let rest: String = text
        .chars()
        .skip_while(|ch| *ch == '$' || ch.is_ascii_alphabetic())
        .take_while(|ch| ch.is_ascii_digit())
        .collect();
    if rest.is_empty() {
        return None;
    }
    let column = col_index(&letters)?;
    let row: u32 = rest.parse().ok()?;
    Some((column, row.checked_sub(1)?))
}

/// The corners of an `A1` or `A1:B2` area, as the top left and the bottom right.
#[must_use]
pub fn parse_area(text: &str) -> Option<(Addr, Addr)> {
    let (first, last) = text.split_once(':').unwrap_or((text, text));
    let first = parse_ref(first)?;
    let last = parse_ref(last)?;
    Some((
        (first.0.min(last.0), first.1.min(last.1)),
        (first.0.max(last.0), first.1.max(last.1)),
    ))
}

/// An area as people write it: `A1`, or `A1:B2` when it spans more than one cell.
#[must_use]
pub fn area_ref((start, end): (Addr, Addr)) -> String {
    if start == end {
        cell_ref(start)
    } else {
        format!("{}:{}", cell_ref(start), cell_ref(end))
    }
}

/// The theme's measurements the grid lays itself out by.
#[derive(Clone, Copy, Debug)]
struct Metrics {
    cell_line_h: f64,
    header_line_h: f64,
    row_label_w: f64,
    pad: f64,
    header_pad: f64,
    control_h: f64,
    scale: f64,
    gutter: f64,
    column_width: f64,
    row_height: f64,
}

impl Metrics {
    fn resolve(tr: &mut TextRenderer, theme: &Theme, column_width: f64, row_height: f64) -> Self {
        let (_, cell_line_h) = tr.measure_style("Ag", None, &theme.text(TextRole::Body));
        let (row_label_w, header_line_h) =
            tr.measure_style("9999", None, &theme.text(TextRole::Caption).numeric());
        let sp = &theme.tokens.spacing;
        Self {
            cell_line_h: f64::from(cell_line_h),
            header_line_h: f64::from(header_line_h),
            row_label_w: f64::from(row_label_w),
            pad: f64::from(sp.s2),
            header_pad: f64::from(sp.s3),
            control_h: f64::from(sp.s16) + f64::from(sp.s4),
            scale: f64::from(theme.text_scale),
            gutter: f64::from(theme.tokens.metrics.scrollbar_gutter),
            column_width,
            row_height,
        }
    }

    fn row_h(&self) -> f64 {
        self.row_height.max(self.cell_line_h + self.pad)
    }

    fn header_h(&self) -> f64 {
        (self.header_line_h + self.header_pad).max(self.control_h)
    }

    fn row_header_w(&self) -> f64 {
        (self.row_label_w + 2.0 * self.pad).max((f64::from(12) + f64::from(2)) * self.scale)
    }

    fn col_w(&self) -> f64 {
        self.column_width.max(f64::from(12) * self.scale)
    }

    fn cell_pad(&self) -> f64 {
        self.pad * self.scale
    }
}

/// What a held pointer is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Drag {
    Select,
    ResizeColumn(u32),
    ResizeRow(u32),
    Fill,
}

/// The cell being edited, and what is typed.
struct Editing {
    addr: Addr,
    text: String,
    core: EditCore,
    /// Started by typing over the cell, so an arrow ends the entry and moves the selection.
    entering: bool,
    /// The text the cell held when the entry began: Escape puts it back.
    before: String,
}

/// What the grid reports to the app.
#[derive(Default)]
pub(crate) struct Events {
    select: crate::component::Event,
    edit: crate::component::Event,
    resized_column: crate::component::Event,
    resized_row: crate::component::Event,
    fill: crate::component::Event,
    copy: crate::component::Event,
    cut: crate::component::Event,
    paste: crate::component::Event,
}

/// A sheet drawn as a grid of cells.
pub struct Sheet {
    rect: Rect,
    metrics: Metrics,
    /// What the sheet is, for assistive technology.
    name: Option<String>,
    columns: u32,
    rows: u32,
    /// Each column's width, by its index; a column absent takes the default.
    widths: HashMap<u32, f64>,
    /// Each row's height, by its index.
    heights: HashMap<u32, f64>,
    cells: HashMap<Addr, SheetCell>,
    found: HashSet<Addr>,
    /// The cells a search in the window found, and what each matched where: its place in the
    /// cell's own text. The search paints these and steps through them; the app's `found` prop
    /// marks cells by its own reason.
    matches: Vec<(Addr, usize, usize)>,
    /// The cells a search found, for the mark under their text.
    searched: HashSet<Addr>,
    /// How many times what the cells hold changed: a search runs again when it moves.
    revision: u64,
    commented: HashSet<Addr>,
    answered: HashSet<Addr>,
    peers: Vec<((Addr, Addr), String)>,
    editable: bool,
    /// The chosen cells: where the choice started, and where it reaches.
    anchor: Addr,
    focus: Addr,
    scroll_x: f64,
    scroll_y: f64,
    drag: Option<Drag>,
    editing: Option<Editing>,
    /// The cell the fill handle was dragged from, while it drags.
    fill_from: Option<Addr>,
    /// Where the pointer is, for the fill handle and resize grabs.
    pointer: Option<Point>,
    bars: (Scrollbar, Scrollbar),
    pub(crate) events: Events,
    focused: bool,
    /// The rows and columns shown last, for the accessibility tree.
    visible: (std::ops::Range<u32>, std::ops::Range<u32>),
    dirty: bool,
}

impl Default for Sheet {
    fn default() -> Self {
        Self::new()
    }
}

impl Sheet {
    /// An empty sheet with one cell chosen.
    #[must_use]
    pub fn new() -> Self {
        Self {
            rect: Rect::ZERO,
            metrics: Metrics {
                cell_line_h: 20.0,
                header_line_h: 16.0,
                row_label_w: 28.0,
                pad: 2.0,
                header_pad: 3.0,
                control_h: 24.0,
                scale: 1.0,
                gutter: 12.0,
                column_width: 96.0,
                row_height: 24.0,
            },
            name: None,
            columns: 26,
            rows: 200,
            widths: HashMap::new(),
            heights: HashMap::new(),
            cells: HashMap::new(),
            found: HashSet::new(),
            matches: Vec::new(),
            searched: HashSet::new(),
            revision: 0,
            commented: HashSet::new(),
            answered: HashSet::new(),
            peers: Vec::new(),
            editable: true,
            anchor: (0, 0),
            focus: (0, 0),
            scroll_x: 0.0,
            scroll_y: 0.0,
            drag: None,
            editing: None,
            fill_from: None,
            pointer: None,
            bars: (Scrollbar::vertical(), Scrollbar::horizontal()),
            events: Events::default(),
            focused: false,
            visible: (0..0, 0..0),
            dirty: true,
        }
    }

    /// Take a sheet's props, answering whether anything it draws changed.
    pub(crate) fn take(&mut self, props: super::SheetProps) -> bool {
        let super::SheetProps {
            name,
            columns,
            rows,
            column_width,
            row_height,
            sizes,
            cells,
            selection,
            found,
            commented,
            answered,
            peers,
            editable,
            select,
            edit,
            resized_column,
            resized_row,
            fill,
            copy,
            cut,
            paste,
        } = props;
        self.events = Events {
            select,
            edit,
            resized_column,
            resized_row,
            fill,
            copy,
            cut,
            paste,
        };
        let mut changed = false;
        if self.name != name {
            self.name = name;
            changed = true;
        }
        let columns = u32::try_from(columns.max(0)).unwrap_or(0).max(1);
        let rows = u32::try_from(rows.max(0)).unwrap_or(0).max(1);
        if (self.columns, self.rows) != (columns, rows) {
            self.columns = columns;
            self.rows = rows;
            changed = true;
        }
        if let Some(width) = column_width {
            if (self.metrics.column_width - width).abs() > f64::EPSILON {
                self.metrics.column_width = width;
                changed = true;
            }
        }
        if let Some(height) = row_height {
            if (self.metrics.row_height - height).abs() > f64::EPSILON {
                self.metrics.row_height = height;
                changed = true;
            }
        }
        let mut widths = HashMap::new();
        let mut heights = HashMap::new();
        for size in &sizes {
            match size {
                SheetSize::Column { column, width } => {
                    if let Some(index) = col_index(column) {
                        widths.insert(index, *width);
                    }
                }
                SheetSize::Row { row, height } => {
                    let index = row.saturating_sub(1);
                    heights.insert(index, *height);
                }
            }
        }
        if self.widths != widths {
            self.widths = widths;
            changed = true;
        }
        if self.heights != heights {
            self.heights = heights;
            changed = true;
        }
        let cells: HashMap<Addr, SheetCell> = cells
            .into_iter()
            .filter_map(|cell| Some((cell.addr()?, cell)))
            .collect();
        if self.cells != cells {
            self.cells = cells;
            self.revision = self.revision.wrapping_add(1);
            changed = true;
        }
        let addresses = |list: Vec<String>| -> HashSet<Addr> {
            list.iter()
                .filter_map(|text| parse_area(text))
                .flat_map(|(start, end)| {
                    (start.1..=end.1)
                        .flat_map(move |row| (start.0..=end.0).map(move |column| (column, row)))
                })
                .collect()
        };
        let found = addresses(found);
        if self.found != found {
            self.found = found;
            changed = true;
        }
        let commented = addresses(commented);
        if self.commented != commented {
            self.commented = commented;
            changed = true;
        }
        let answered = addresses(answered);
        if self.answered != answered {
            self.answered = answered;
            changed = true;
        }
        let peers: Vec<((Addr, Addr), String)> = peers
            .into_iter()
            .filter_map(|peer| Some((parse_area(&peer.area)?, peer.who)))
            .collect();
        if self.peers != peers {
            self.peers = peers;
            changed = true;
        }
        if self.editable != editable {
            self.editable = editable;
            changed = true;
        }
        // The app's selection wins, unless the person is dragging or typing in one cell.
        if self.drag != Some(Drag::Select) && self.editing.is_none() {
            if let Some(area) = selection.as_deref().and_then(parse_area) {
                if (self.anchor, self.focus) != area {
                    self.anchor = area.0;
                    self.focus = area.1;
                    changed = true;
                }
            }
        }
        if changed {
            self.dirty = true;
        }
        changed
    }

    /// The chosen area.
    fn selection(&self) -> (Addr, Addr) {
        (
            (
                self.anchor.0.min(self.focus.0),
                self.anchor.1.min(self.focus.1),
            ),
            (
                self.anchor.0.max(self.focus.0),
                self.anchor.1.max(self.focus.1),
            ),
        )
    }

    fn column_x(&self, column: u32) -> f64 {
        let mut x = 0.0;
        for index in 0..column {
            x += self
                .widths
                .get(&index)
                .copied()
                .unwrap_or_else(|| self.metrics.col_w());
        }
        x
    }

    fn row_y(&self, row: u32) -> f64 {
        let mut y = 0.0;
        for index in 0..row {
            y += self
                .heights
                .get(&index)
                .copied()
                .unwrap_or_else(|| self.metrics.row_h());
        }
        y
    }

    fn column_w(&self, column: u32) -> f64 {
        self.widths
            .get(&column)
            .copied()
            .unwrap_or_else(|| self.metrics.col_w())
    }

    fn row_h(&self, row: u32) -> f64 {
        self.heights
            .get(&row)
            .copied()
            .unwrap_or_else(|| self.metrics.row_h())
    }

    /// The strip the column headers take.
    fn header_h(&self) -> f64 {
        self.metrics.header_h()
    }

    /// The strip the row headers take.
    fn header_w(&self) -> f64 {
        self.metrics.row_header_w()
    }

    /// The cells' own box: what the headers frame.
    fn body(&self) -> Rect {
        Rect::new(
            self.rect.x0 + self.header_w(),
            self.rect.y0 + self.header_h(),
            self.rect.x1,
            self.rect.y1,
        )
    }

    /// The body's box minus the scrollbars' gutter.
    fn viewport(&self) -> Rect {
        let body = self.body();
        Rect::new(
            body.x0,
            body.y0,
            (body.x1 - self.metrics.gutter).max(body.x0),
            (body.y1 - self.metrics.gutter).max(body.y0),
        )
    }

    /// Where a cell is drawn.
    fn cell_rect(&self, addr: Addr) -> Rect {
        let body = self.body();
        let x = body.x0 + self.column_x(addr.0) - self.scroll_x;
        let y = body.y0 + self.row_y(addr.1) - self.scroll_y;
        Rect::new(x, y, x + self.column_w(addr.0), y + self.row_h(addr.1))
    }

    /// The cell under a point in the body, if any.
    fn cell_at(&self, point: Point) -> Option<Addr> {
        let body = self.body();
        if !body.contains(point) {
            return None;
        }
        let (column, _) = self.line_at(point.x - body.x0 + self.scroll_x, true)?;
        let (row, _) = self.line_at(point.y - body.y0 + self.scroll_y, false)?;
        Some((column, row))
    }

    /// The column (or row) at `offset` into the scrolled axis, and its start.
    fn line_at(&self, offset: f64, horizontal: bool) -> Option<(u32, f64)> {
        let mut start = 0.0;
        let count = if horizontal { self.columns } else { self.rows };
        for index in 0..count {
            let length = if horizontal {
                self.column_w(index)
            } else {
                self.row_h(index)
            };
            if offset < start + length {
                return Some((index, start));
            }
            start += length;
        }
        None
    }

    /// The scroll offset past which nothing is left to scroll to.
    fn max_scroll(&self) -> (f64, f64) {
        let viewport = self.viewport();
        (
            (self.column_x(self.columns) - viewport.width()).max(0.0),
            (self.row_y(self.rows) - viewport.height()).max(0.0),
        )
    }

    /// Scroll so `addr` is in the body.
    fn reveal(&mut self, addr: Addr) {
        let viewport = self.viewport();
        let x = self.column_x(addr.0);
        let w = self.column_w(addr.0);
        if x < self.scroll_x {
            self.scroll_x = x;
        } else if x + w > self.scroll_x + viewport.width() {
            self.scroll_x = x + w - viewport.width();
        }
        let y = self.row_y(addr.1);
        let h = self.row_h(addr.1);
        if y < self.scroll_y {
            self.scroll_y = y;
        } else if y + h > self.scroll_y + viewport.height() {
            self.scroll_y = y + h - viewport.height();
        }
        let (max_x, max_y) = self.max_scroll();
        self.scroll_x = self.scroll_x.clamp(0.0, max_x);
        self.scroll_y = self.scroll_y.clamp(0.0, max_y);
        self.dirty = true;
    }

    /// Choose `addr`, extending from the anchor when `extend`.
    fn choose(&mut self, addr: Addr, extend: bool) {
        if !extend {
            self.anchor = addr;
        }
        self.focus = addr;
        self.dirty = true;
        self.reveal(addr);
        self.fire_select();
    }

    fn fire_select(&self) {
        self.events
            .select
            .fire(Some(crate::component::Value::Text(area_ref(
                self.selection(),
            ))));
    }

    /// Report a written cell as a pair of reference and input.
    fn fire_edit(&self, writes: Vec<(Addr, String)>) {
        if writes.is_empty() {
            return;
        }
        let mut values = Vec::with_capacity(writes.len() * 2);
        for (addr, input) in writes {
            values.push(crate::component::Value::Text(cell_ref(addr)));
            values.push(crate::component::Value::Text(input));
        }
        self.events
            .edit
            .fire(Some(crate::component::Value::List(values)));
    }

    /// The input the cell holds in the app's data, as the editor starts from it.
    fn cell_input(&self, addr: Addr) -> String {
        // The app hands the grid what the cell shows and its raw input is not part of that; the
        // editor starts from what it shows, which is what a person edits unless the app opens
        // the formula bar instead.
        self.cells
            .get(&addr)
            .map_or_else(String::new, |cell| cell.text.clone())
    }

    /// Start editing `addr`, from the cell's text or from `typed` when the person typed over it.
    fn begin_edit(&mut self, addr: Addr, typed: Option<&str>) {
        if !self.editable {
            return;
        }
        let (text, entering) = match typed {
            Some(typed) => (typed.to_owned(), true),
            None => (self.cell_input(addr), false),
        };
        let mut core = EditCore::new();
        core.select_all(&text);
        if let Some(typed) = typed {
            // Typing replaces the cell, so the caret stands after what was typed.
            core = EditCore::at(typed.len());
        }
        self.editing = Some(Editing {
            addr,
            before: self.cell_input(addr),
            text,
            core,
            entering,
        });
        self.anchor = addr;
        self.focus = addr;
        self.dirty = true;
    }

    /// Finish the entry, writing what it holds, and move the selection as `move_to` says.
    fn commit_edit(&mut self, move_to: Option<Addr>) {
        let Some(editing) = self.editing.take() else {
            return;
        };
        self.dirty = true;
        if editing.text != editing.before {
            self.fire_edit(vec![(editing.addr, editing.text)]);
        }
        if let Some(addr) = move_to {
            let addr = (
                addr.0.min(self.columns.saturating_sub(1)),
                addr.1.min(self.rows.saturating_sub(1)),
            );
            self.choose(addr, false);
        }
    }

    /// Leave the entry as it was.
    fn cancel_edit(&mut self) {
        if self.editing.take().is_some() {
            self.dirty = true;
        }
    }

    /// The cells the focus moves to with an arrow, as `(d column, d row)`.
    fn step(&mut self, d_col: i64, d_row: i64, extend: bool) {
        let clamp = |value: i64, max: u32| -> u32 {
            u32::try_from(value.clamp(0, i64::from(max.saturating_sub(1)))).unwrap_or(0)
        };
        let next = (
            clamp(i64::from(self.focus.0) + d_col, self.columns),
            clamp(i64::from(self.focus.1) + d_row, self.rows),
        );
        self.choose(next, extend);
    }

    /// The scroll's page: how many rows the body holds.
    fn page_rows(&self) -> u32 {
        let h = self.metrics.row_h();
        u32::try_from((self.viewport().height() / h).floor().max(1.0) as u32).unwrap_or(1)
    }

    /// Which rows and columns the body shows.
    fn visible_lines(&self) -> (std::ops::Range<u32>, std::ops::Range<u32>) {
        let viewport = self.viewport();
        let columns = self
            .line_at(self.scroll_x, true)
            .map_or(0, |(index, _)| index)
            ..self
                .line_at(self.scroll_x + viewport.width(), true)
                .map_or(self.columns, |(index, _)| index.saturating_add(1));
        let rows = self
            .line_at(self.scroll_y, false)
            .map_or(0, |(index, _)| index)
            ..self
                .line_at(self.scroll_y + viewport.height(), false)
                .map_or(self.rows, |(index, _)| index.saturating_add(1));
        (columns, rows)
    }

    /// Move the scrollbars' thumbs to where the body stands.
    fn bar_metrics(&self, vertical: bool) -> ScrollMetrics {
        let viewport = self.viewport();
        if vertical {
            ScrollMetrics::new(self.row_y(self.rows), viewport.height(), self.scroll_y)
        } else {
            ScrollMetrics::new(self.column_x(self.columns), viewport.width(), self.scroll_x)
        }
    }

    /// Scroll by the wheel.
    fn wheel(&mut self, delta: &winit::event::MouseScrollDelta, modifiers: Modifiers) {
        let (dx, dy) = match delta {
            winit::event::MouseScrollDelta::LineDelta(x, y) => {
                (f64::from(*x) * 48.0, f64::from(*y) * 48.0)
            }
            winit::event::MouseScrollDelta::PixelDelta(p) => (p.x, p.y),
        };
        let shift = modifiers.state().shift_key();
        if shift {
            self.scroll_x -= dy;
        } else {
            self.scroll_x -= dx;
            self.scroll_y -= dy;
        }
        let (max_x, max_y) = self.max_scroll();
        self.scroll_x = self.scroll_x.clamp(0.0, max_x);
        self.scroll_y = self.scroll_y.clamp(0.0, max_y);
        self.dirty = true;
    }

    /// The keys the grid handles itself, before the window takes them.
    fn key(&mut self, event: &KeyEvent, modifiers: Modifiers) -> bool {
        let shift = modifiers.state().shift_key();
        let control = modifiers.state().control_key() || modifiers.state().super_key();
        let key = &event.logical_key;
        if self.editing.is_some() {
            return self.edit_key(event, shift, control);
        }
        match key {
            Key::Named(NamedKey::ArrowUp) => {
                self.step(0, -1, shift);
                true
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.step(0, 1, shift);
                true
            }
            Key::Named(NamedKey::ArrowLeft) => {
                self.step(-1, 0, shift);
                true
            }
            Key::Named(NamedKey::ArrowRight) => {
                self.step(1, 0, shift);
                true
            }
            Key::Named(NamedKey::Tab) => {
                self.step(if shift { -1 } else { 1 }, 0, false);
                true
            }
            Key::Named(NamedKey::Enter | NamedKey::F2) => {
                self.begin_edit(self.focus, None);
                true
            }
            Key::Named(NamedKey::Home) => {
                self.choose((0, self.focus.1), false);
                true
            }
            Key::Named(NamedKey::End) => {
                let column = self.last_used_column().max(self.focus.0);
                self.choose((column, self.focus.1), false);
                true
            }
            Key::Named(NamedKey::PageDown) => {
                let rows = self.page_rows();
                self.step(0, i64::from(rows), shift);
                true
            }
            Key::Named(NamedKey::PageUp) => {
                let rows = self.page_rows();
                self.step(0, -i64::from(rows), shift);
                true
            }
            Key::Named(NamedKey::Delete | NamedKey::Backspace) => {
                self.clear_selection();
                true
            }
            Key::Named(NamedKey::Escape) => {
                self.cancel_edit();
                true
            }
            Key::Character(text) if control => {
                if text.eq_ignore_ascii_case("a") {
                    self.anchor = (0, 0);
                    self.focus = (self.columns.saturating_sub(1), self.rows.saturating_sub(1));
                    self.dirty = true;
                    self.fire_select();
                    return true;
                }
                // The person's clipboard is the app's to write: the grid reports what to copy,
                // cut or take, and the app's action declares `uses clipboard`.
                if text.eq_ignore_ascii_case("c") {
                    self.events
                        .copy
                        .fire(Some(crate::component::Value::Text(self.copy_selection())));
                    return true;
                }
                if text.eq_ignore_ascii_case("x") {
                    let cut = self.copy_selection();
                    self.events
                        .cut
                        .fire(Some(crate::component::Value::Text(cut)));
                    return true;
                }
                if text.eq_ignore_ascii_case("v") {
                    self.events.paste.fire(None);
                    return true;
                }
                false
            }
            Key::Character(text) if !control && !modifiers.state().alt_key() => {
                let typed = text.to_string();
                self.begin_edit(self.focus, Some(&typed));
                true
            }
            _ => false,
        }
    }

    /// The last column that holds anything, for End.
    fn last_used_column(&self) -> u32 {
        self.cells.keys().map(|addr| addr.0).max().unwrap_or(0)
    }

    /// The keys while a cell is being edited.
    fn edit_key(&mut self, event: &KeyEvent, shift: bool, control: bool) -> bool {
        let Some(editing) = self.editing.as_mut() else {
            return false;
        };
        let mut text = std::mem::take(&mut editing.text);
        let mut motion = |m: Motion| {
            editing.core.motion(&text, m, shift);
            true
        };
        let handled = match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                editing.text = text;
                self.cancel_edit();
                return true;
            }
            Key::Named(NamedKey::Enter) => {
                editing.text = text;
                let down = self.focus.1.saturating_add(1);
                self.commit_edit(Some((self.focus.0, down)));
                return true;
            }
            Key::Named(NamedKey::Tab) => {
                editing.text = text;
                let right = if shift {
                    self.focus.0.saturating_sub(1)
                } else {
                    self.focus.0.saturating_add(1)
                };
                self.commit_edit(Some((right, self.focus.1)));
                return true;
            }
            Key::Named(NamedKey::Backspace) => {
                let edit = if editing.core.has_selection() {
                    editing.core.delete_selection()
                } else {
                    editing.core.delete_backward(&text, control)
                };
                edit.apply(&mut text);
                editing.text = text;
                true
            }
            Key::Named(NamedKey::Delete) => {
                let edit = if editing.core.has_selection() {
                    editing.core.delete_selection()
                } else {
                    editing.core.delete_forward(&text, control)
                };
                edit.apply(&mut text);
                editing.text = text;
                true
            }
            Key::Named(NamedKey::ArrowLeft) => {
                if editing.entering && !shift && !control {
                    editing.text = text;
                    let left = self.focus.0.saturating_sub(1);
                    self.commit_edit(Some((left, self.focus.1)));
                    return true;
                }
                motion(if control {
                    Motion::WordLeft
                } else {
                    Motion::Left
                })
            }
            Key::Named(NamedKey::ArrowRight) => {
                if editing.entering && !shift && !control {
                    editing.text = text;
                    let right = self.focus.0.saturating_add(1);
                    self.commit_edit(Some((right, self.focus.1)));
                    return true;
                }
                motion(if control {
                    Motion::WordRight
                } else {
                    Motion::Right
                })
            }
            Key::Named(NamedKey::ArrowUp) => {
                if editing.entering {
                    editing.text = text;
                    let up = self.focus.1.saturating_sub(1);
                    self.commit_edit(Some((self.focus.0, up)));
                    return true;
                }
                motion(Motion::LineUp)
            }
            Key::Named(NamedKey::ArrowDown) => {
                if editing.entering {
                    editing.text = text;
                    let down = self.focus.1.saturating_add(1);
                    self.commit_edit(Some((self.focus.0, down)));
                    return true;
                }
                motion(Motion::LineDown)
            }
            Key::Named(NamedKey::Home) => motion(Motion::LineStart),
            Key::Named(NamedKey::End) => motion(Motion::LineEnd),
            Key::Character(text_typed) if control => {
                if text_typed.eq_ignore_ascii_case("a") {
                    editing.core.select_all(&text);
                }
                false
            }
            Key::Character(text_typed) => {
                let typed = text_typed.to_string();
                let edit = editing.core.insert(&typed);
                edit.apply(&mut text);
                editing.text = text;
                true
            }
            Key::Named(NamedKey::Space) => {
                let edit = editing.core.insert(" ");
                edit.apply(&mut text);
                editing.text = text;
                true
            }
            _ => {
                editing.text = text;
                false
            }
        };
        if let Some(editing) = &self.editing {
            self.dirty |= !editing.text.is_empty();
        }
        self.dirty = true;
        handled
    }

    /// Empty every chosen cell but the one the selection started at, which the editor shows.
    fn clear_selection(&self) {
        let (start, end) = self.selection();
        let mut writes = Vec::new();
        for row in start.1..=end.1 {
            for column in start.0..=end.0 {
                let addr = (column, row);
                if self.cells.contains_key(&addr) {
                    writes.push((addr, String::new()));
                }
            }
        }
        self.fire_edit(writes);
    }

    /// Copy the chosen cells as the tabs and lines a table pastes as.
    fn copy_selection(&self) -> String {
        let (start, end) = self.selection();
        let mut out = String::new();
        for row in start.1..=end.1 {
            for column in start.0..=end.0 {
                if column > start.0 {
                    out.push('\t');
                }
                let addr = (column, row);
                let editing = self
                    .editing
                    .as_ref()
                    .filter(|editing| editing.addr == addr)
                    .map(|editing| editing.text.clone());
                out.push_str(&editing.unwrap_or_else(|| self.cell_input(addr)));
            }
            out.push('\n');
        }
        out
    }

    /// Where the fill handle is drawn: the selection's bottom right corner.
    fn fill_handle_rect(&self) -> Rect {
        let (_, end) = self.selection();
        let cell = self.cell_rect(end);
        let side = f64::from(8);
        Rect::new(
            cell.x1 - side / 2.0,
            cell.y1 - side / 2.0,
            cell.x1 + side / 2.0,
            cell.y1 + side / 2.0,
        )
    }

    /// A header edge's grab: resizing a column or a row.
    fn resize_grab(&self, point: Point) -> Option<Drag> {
        let body = self.body();
        let header_h = self.header_h();
        let header_w = self.header_w();
        if point.y >= self.rect.y0 && point.y <= self.rect.y0 + header_h && point.x >= body.x0 {
            let (column, start) = self.line_at(point.x - body.x0 + self.scroll_x, true)?;
            let edge = body.x0 + start + self.column_w(column) - self.scroll_x;
            if (point.x - edge).abs() <= f64::from(3) {
                return Some(Drag::ResizeColumn(column));
            }
        }
        if point.x >= self.rect.x0 && point.x <= self.rect.x0 + header_w && point.y >= body.y0 {
            let (row, start) = self.line_at(point.y - body.y0 + self.scroll_y, false)?;
            let edge = body.y0 + start + self.row_h(row) - self.scroll_y;
            if (point.y - edge).abs() <= f64::from(3) {
                return Some(Drag::ResizeRow(row));
            }
        }
        None
    }
}

impl Widget for Sheet {
    crate::described!();

    fn name(&self) -> &'static str {
        "Sheet"
    }

    crate::provides!(dyn Findable);

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn crate::widget::Widget>,
    ) -> NodeId {
        self.metrics.scale = f64::from(theme.text_scale);
        let style = taffy::Style {
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::percent(1.0),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    /// Its size is its slot's, from its style alone: it asks for a layout only until it has
    /// one, and what it shows inside that slot is placed as it draws.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let rect = crate::widget::layout_rect(taffy, node, origin);
        if rect != self.rect {
            self.rect = rect;
            self.dirty = true;
        }
    }

    fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        self.metrics = Metrics::resolve(
            text_renderer,
            theme,
            self.metrics.column_width,
            self.metrics.row_height,
        );
        self.visible = self.visible_lines();
        let colors = &theme.tokens.colors;
        let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);
        // The ground under the cells and the headers it frames.
        ctx.fill_rect(self.rect, colors.surfaces.content);
        let body = self.body();
        let (columns, rows) = self.visible.clone();
        // Selection tint, then the cells, then the lines over them.
        let (start, end) = self.selection();
        for row in rows.clone() {
            if row < start.1 || row > end.1 {
                continue;
            }
            for column in columns.clone() {
                if column < start.0 || column > end.0 {
                    continue;
                }
                let rect = self.cell_rect((column, row)).intersect(body);
                if rect.width() > 0.0 && rect.height() > 0.0 {
                    ctx.fill_rect(rect, colors.intent.selection_tint);
                }
            }
        }
        for &((peer_start, peer_end), _) in &self.peers {
            for row in rows.clone() {
                if row < peer_start.1 || row > peer_end.1 {
                    continue;
                }
                for column in columns.clone() {
                    if column < peer_start.0 || column > peer_end.0 {
                        continue;
                    }
                    let rect = self.cell_rect((column, row)).intersect(body);
                    if rect.width() > 0.0 && rect.height() > 0.0 {
                        ctx.fill_rect(rect, colors.intent.selection_tint);
                    }
                }
            }
        }
        // The cells.
        let style = theme.text(TextRole::Body);
        for row in rows.clone() {
            for column in columns.clone() {
                let addr = (column, row);
                let Some(cell) = self.cells.get(&addr) else {
                    continue;
                };
                let Some(rect) = Some(self.cell_rect(addr).intersect(body)) else {
                    continue;
                };
                if rect.width() <= 0.0 || rect.height() <= 0.0 {
                    continue;
                }
                if cell.text.is_empty() {
                    continue;
                }
                // The editor paints the cell it edits.
                if self
                    .editing
                    .as_ref()
                    .is_some_and(|editing| editing.addr == addr)
                {
                    continue;
                }
                self.draw_text(&mut ctx, &cell.text, &style, cell, rect, body);
            }
        }
        self.draw_editor(&mut ctx, theme);
        self.draw_lines(&mut ctx, theme, &columns, &rows);
        self.draw_headers(&mut ctx, theme, &columns, &rows);
        self.draw_found(&mut ctx, theme, &columns, &rows);
        self.draw_selection(&mut ctx, theme);
        self.draw_bars(&mut ctx, theme);
        self.draw_fill_handle(&mut ctx, theme);
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.bars.0.is_active() || self.bars.1.is_active()
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        if self.needs_redraw() {
            regions.push(self.rect);
        }
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        if self.focused != focused {
            self.focused = focused;
            self.dirty = true;
        }
        if !focused {
            self.commit_edit(None);
        }
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn wants_pointer_capture(&self) -> bool {
        self.drag.is_some()
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if matches!(self.resize_grab(point), Some(Drag::ResizeColumn(_))) {
            return Some(winit::window::CursorIcon::ColResize);
        }
        if matches!(self.resize_grab(point), Some(Drag::ResizeRow(_))) {
            return Some(winit::window::CursorIcon::RowResize);
        }
        None
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let point = ctx.logical(*position);
                self.pointer = Some(point);
                match self.drag {
                    Some(Drag::Select) => {
                        if let Some(addr) = self.cell_at(point) {
                            if addr != self.focus {
                                self.focus = addr;
                                self.dirty = true;
                                self.fire_select();
                            }
                        }
                    }
                    Some(Drag::ResizeColumn(column)) => {
                        let body = self.body();
                        let (_, start) = self
                            .line_at(self.column_x(column), true)
                            .unwrap_or((column, 0.0));
                        let width = (point.x - body.x0 - start + self.scroll_x).max(f64::from(16));
                        self.widths.insert(column, width);
                        self.dirty = true;
                        self.events
                            .resized_column
                            .fire(Some(crate::component::Value::List(vec![
                                crate::component::Value::Text(col_letters(column)),
                                crate::component::Value::Text(format!("{width:.0}")),
                            ])));
                    }
                    Some(Drag::ResizeRow(row)) => {
                        let body = self.body();
                        let (_, start) = self.line_at(self.row_y(row), false).unwrap_or((row, 0.0));
                        let height = (point.y - body.y0 - start + self.scroll_y).max(f64::from(12));
                        self.heights.insert(row, height);
                        self.dirty = true;
                        self.events
                            .resized_row
                            .fire(Some(crate::component::Value::List(vec![
                                crate::component::Value::Text((row + 1).to_string()),
                                crate::component::Value::Text(format!("{height:.0}")),
                            ])));
                    }
                    Some(Drag::Fill) => self.dirty = true,
                    None => {
                        if self
                            .fill_handle_rect()
                            .inflate(f64::from(4), f64::from(4))
                            .contains(point)
                        {
                            *ctx.handled = true;
                        }
                    }
                }
            }
            WindowEvent::CursorLeft { .. } => {
                self.pointer = None;
                self.dirty = true;
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => match state {
                ElementState::Pressed => {
                    let Some(point) = self.pointer else {
                        return;
                    };
                    *ctx.focus_target = Some(ctx.node_id);
                    if let Some(grab) = self.resize_grab(point) {
                        self.drag = Some(grab);
                        *ctx.handled = true;
                        return;
                    }
                    if self
                        .fill_handle_rect()
                        .inflate(f64::from(4), f64::from(4))
                        .contains(point)
                    {
                        self.commit_edit(None);
                        self.fill_from = Some(self.selection().1);
                        self.drag = Some(Drag::Fill);
                        *ctx.handled = true;
                        return;
                    }
                    let header_h = self.rect.y0 + self.header_h();
                    let header_w = self.rect.x0 + self.header_w();
                    if point.y < header_h && point.x >= header_w {
                        // A column header chooses its column.
                        if let Some((column, _)) =
                            self.line_at(point.x - header_w + self.scroll_x, true)
                        {
                            self.anchor = (column, 0);
                            self.focus = (column, self.rows.saturating_sub(1));
                            self.dirty = true;
                            self.fire_select();
                            *ctx.handled = true;
                            return;
                        }
                    }
                    if point.x < header_w && point.y >= header_h {
                        if let Some((row, _)) =
                            self.line_at(point.y - header_h + self.scroll_y, false)
                        {
                            self.anchor = (0, row);
                            self.focus = (self.columns.saturating_sub(1), row);
                            self.dirty = true;
                            self.fire_select();
                            *ctx.handled = true;
                            return;
                        }
                    }
                    if let Some(addr) = self.cell_at(point) {
                        let extend = ctx.modifiers.state().shift_key();
                        self.commit_edit(None);
                        self.choose(addr, extend);
                        self.drag = Some(Drag::Select);
                        *ctx.handled = true;
                    }
                }
                ElementState::Released => {
                    if self.drag == Some(Drag::Fill) {
                        let target = self.pointer.and_then(|point| self.cell_at(point));
                        let source = self.fill_from.take();
                        self.drag = None;
                        if let (Some(source), Some(target)) = (source, target) {
                            let (start, _) = self.selection();
                            let from = area_ref((start, source));
                            let to = area_ref((start, target));
                            if from != to {
                                self.events
                                    .fill
                                    .fire(Some(crate::component::Value::List(vec![
                                        crate::component::Value::Text(from),
                                        crate::component::Value::Text(to),
                                    ])));
                            }
                        }
                        self.dirty = true;
                        *ctx.handled = true;
                        return;
                    }
                    if self.drag.is_some() {
                        self.drag = None;
                        self.bars.0.release();
                        self.bars.1.release();
                        *ctx.handled = true;
                    }
                }
            },
            WindowEvent::MouseWheel { delta, .. } => {
                self.wheel(delta, *ctx.modifiers);
                *ctx.handled = true;
            }
            WindowEvent::Ime(Ime::Commit(text)) => {
                if let Some(editing) = self.editing.as_mut() {
                    let mut current = std::mem::take(&mut editing.text);
                    let edit = editing.core.insert(text);
                    edit.apply(&mut current);
                    editing.text = current;
                    self.dirty = true;
                    *ctx.handled = true;
                }
            }
            WindowEvent::KeyboardInput { event: key, .. }
                if key.state.is_pressed() && self.key(key, *ctx.modifiers) =>
            {
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    fn wants_tab_input(&self) -> bool {
        true
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        let editing = self.editing.as_ref()?;
        Some(self.cell_rect(editing.addr))
    }

    fn ime_content(&self) -> Option<crate::ime::InputContent> {
        self.editing
            .as_ref()
            .map(|_| crate::ime::InputContent::TEXT)
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        let (start, end) = self.selection();
        let (columns, rows) = self.visible.clone();
        let mut row_nodes = Vec::new();
        for row in rows {
            let mut cells = Vec::new();
            for column in columns.clone() {
                let addr = (column, row);
                let reference = cell_ref(addr);
                let value = self.cells.get(&addr).map(|cell| cell.text.clone());
                let selected =
                    (start.0..=end.0).contains(&column) && (start.1..=end.1).contains(&row);
                cells.push(
                    AccessNode::new(Role::Cell)
                        .with_name(reference)
                        .with_optional_value(value.filter(|text| !text.is_empty()))
                        .with_position(column as usize + 1, self.columns as usize)
                        .with_bounds(self.cell_rect(addr))
                        .with_state(
                            AccessState::new()
                                .selected(selected)
                                .focused(addr == self.focus),
                        ),
                );
            }
            row_nodes.push(
                AccessNode::new(Role::Row)
                    .with_name(format!("Row {}", row + 1))
                    .with_position(row as usize + 1, self.rows as usize)
                    .with_bounds(self.cell_rect((0, row)))
                    .with_children(cells),
            );
        }
        Some(
            AccessNode::new(Role::Table)
                .with_name(self.name.clone().unwrap_or_else(|| "Sheet".to_owned()))
                .with_value(format!("Active cell {}", cell_ref(self.focus)))
                .with_bounds(self.rect)
                .with_state(AccessState::new().focusable(true).focused(self.focused))
                .with_children(row_nodes),
        )
    }
}

impl Sheet {
    /// One cell's text, elided at its cell and its body, aligned as its kind reads.
    fn draw_text(
        &self,
        ctx: &mut PaintCtx<'_>,
        text: &str,
        style: &TextStyle,
        cell: &SheetCell,
        rect: Rect,
        body: Rect,
    ) {
        let pad = self.metrics.cell_pad();
        let right = matches!(cell.kind, CellKind::Number) || matches!(cell.kind, CellKind::Bool);
        let left = rect.x0 + pad;
        let right_edge = rect.x1.min(body.x1) - pad;
        let room = if right {
            rect.width() - 2.0 * pad
        } else {
            right_edge - left
        };
        if room <= 1.0 {
            return;
        }
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a cell's width is inside f32's exact range"
        )]
        let room = room as f32;
        let shown = ctx.text.elide_to_width_style(text, room, style);
        let (width, height) = ctx.text.measure_style(&shown, None, style);
        let top = rect.y0 + (rect.height() - f64::from(height)) / 2.0;
        let x = if right {
            rect.x1 - pad - f64::from(width)
        } else {
            left
        };
        let color = if matches!(cell.kind, CellKind::Error) {
            ctx.theme.tokens.colors.semantic.danger
        } else {
            ctx.theme.tokens.colors.text_primary
        };
        let mut style = style.clone();
        if cell.bold {
            style.weight = TextWeight::Bold;
        }
        ctx.text(&shown, &style, color, Point::new(x, top), None);
    }

    /// The editor's own field, over the cell it edits.
    fn draw_editor(&self, ctx: &mut PaintCtx<'_>, theme: &Theme) {
        let Some(editing) = &self.editing else {
            return;
        };
        let rect = self.cell_rect(editing.addr);
        if rect.intersect(self.body()).is_zero_area() {
            return;
        }
        ctx.fill_rect(rect, theme.tokens.colors.surfaces.content);
        ctx.stroke_rect(rect, theme.tokens.colors.intent.accent_ink, 2.0);
        let style = theme.text(TextRole::Body);
        let pad = self.metrics.cell_pad();
        let (_, height) = ctx.text.measure_style("Ag", None, &style);
        let top = rect.y0 + (rect.height() - f64::from(height)) / 2.0;
        ctx.text(
            &editing.text,
            &style,
            theme.tokens.colors.text_primary,
            Point::new(rect.x0 + pad, top),
            None,
        );
    }

    /// The grid's hairlines.
    fn draw_lines(
        &self,
        ctx: &mut PaintCtx<'_>,
        theme: &Theme,
        columns: &std::ops::Range<u32>,
        rows: &std::ops::Range<u32>,
    ) {
        let body = self.body();
        let color = crate::surface::edge_on(theme.tokens.colors.surfaces.content);
        let stroke = Stroke::new(1.0);
        for &column in &columns.clone().collect::<Vec<_>>() {
            let x = (body.x0 + self.column_x(column) + self.column_w(column) - self.scroll_x)
                .round()
                + 0.5;
            if (body.x0..=body.x1).contains(&x) {
                ctx.scene.stroke(
                    &stroke,
                    ctx.transform,
                    color,
                    None,
                    &Line::new((x, body.y0), (x, body.y1)),
                );
            }
        }
        for &row in &rows.clone().collect::<Vec<_>>() {
            let y = (body.y0 + self.row_y(row) + self.row_h(row) - self.scroll_y).round() + 0.5;
            if (body.y0..=body.y1).contains(&y) {
                ctx.scene.stroke(
                    &stroke,
                    ctx.transform,
                    color,
                    None,
                    &Line::new((body.x0, y), (body.x1, y)),
                );
            }
        }
    }

    /// The column letters and row numbers, in the window's sidebar role.
    fn draw_headers(
        &self,
        ctx: &mut PaintCtx<'_>,
        theme: &Theme,
        columns: &std::ops::Range<u32>,
        rows: &std::ops::Range<u32>,
    ) {
        let body = self.body();
        let colors = &theme.tokens.colors;
        let header_h = self.header_h();
        let header_w = self.header_w();
        let col_strip = Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x1,
            self.rect.y0 + header_h,
        );
        let row_strip = Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x0 + header_w,
            self.rect.y1,
        );
        let corner = Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x0 + header_w,
            self.rect.y0 + header_h,
        );
        crate::surface::region(ctx.scene, ctx.transform, col_strip, Region::Sidebar, theme);
        crate::surface::region(ctx.scene, ctx.transform, row_strip, Region::Sidebar, theme);
        crate::surface::region(ctx.scene, ctx.transform, corner, Region::Sidebar, theme);
        let style = theme.text(TextRole::Caption).numeric();
        let (start, end) = self.selection();
        // A chosen column or row reads lifted in the header.
        for column in columns.clone() {
            let x = body.x0 + self.column_x(column) - self.scroll_x;
            let w = self.column_w(column);
            let band = Rect::new(x, self.rect.y0, x + w, self.rect.y0 + header_h);
            if band.x1 < body.x0 || band.x0 > body.x1 {
                continue;
            }
            if (start.0..=end.0).contains(&column)
                && start.1 == 0
                && end.1 == self.rows.saturating_sub(1)
            {
                ctx.fill_rect(band, colors.intent.selection_tint);
            }
            let label = col_letters(column);
            let (width, height) = ctx.text.measure_style(&label, None, &style);
            let tx = x + (w - f64::from(width)) / 2.0;
            let ty = self.rect.y0 + (header_h - f64::from(height)) / 2.0;
            ctx.text(
                &label,
                &style,
                colors.text_dim,
                Point::new(tx.max(body.x0), ty),
                None,
            );
        }
        for row in rows.clone() {
            let y = body.y0 + self.row_y(row) - self.scroll_y;
            let h = self.row_h(row);
            let band = Rect::new(self.rect.x0, y, self.rect.x0 + header_w, y + h);
            if band.y1 < body.y0 || band.y0 > body.y1 {
                continue;
            }
            if (start.1..=end.1).contains(&row)
                && start.0 == 0
                && end.0 == self.columns.saturating_sub(1)
            {
                ctx.fill_rect(band, colors.intent.selection_tint);
            }
            let label = (row + 1).to_string();
            let (width, height) = ctx.text.measure_style(&label, None, &style);
            let tx = self.rect.x0 + header_w - f64::from(width) - self.metrics.cell_pad();
            let ty = y + (h - f64::from(height)) / 2.0;
            ctx.text(
                &label,
                &style,
                colors.text_dim,
                Point::new(tx, ty.max(body.y0)),
                None,
            );
        }
        let rule = edge_on(theme.region(Region::Sidebar));
        ctx.scene.fill(
            Fill::NonZero,
            ctx.transform,
            rule,
            None,
            &Rect::new(self.rect.x0, body.y0 - 1.0, self.rect.x1, body.y0),
        );
        ctx.scene.fill(
            Fill::NonZero,
            ctx.transform,
            rule,
            None,
            &Rect::new(body.x0 - 1.0, self.rect.y0, body.x0, self.rect.y1),
        );
    }

    /// The cells the window's search found, marked under their text.
    fn draw_found(
        &self,
        ctx: &mut PaintCtx<'_>,
        theme: &Theme,
        columns: &std::ops::Range<u32>,
        rows: &std::ops::Range<u32>,
    ) {
        if self.found.is_empty() && self.searched.is_empty() {
            return;
        }
        let body = self.body();
        for row in rows.clone() {
            for column in columns.clone() {
                if !self.found.contains(&(column, row)) && !self.searched.contains(&(column, row)) {
                    continue;
                }
                let rect = self.cell_rect((column, row)).intersect(body);
                if rect.is_zero_area() {
                    continue;
                }
                ctx.fill_rect(
                    Rect::new(rect.x0 + 1.0, rect.y1 - 2.0, rect.x1 - 1.0, rect.y1),
                    theme.tokens.colors.intent.accent_ink,
                );
            }
        }
    }

    /// The selection's outline, the active cell, a comment's corner mark and others' choices.
    fn draw_selection(&self, ctx: &mut PaintCtx<'_>, theme: &Theme) {
        let body = self.body();
        let colors = &theme.tokens.colors;
        let (start, end) = self.selection();
        let top_left = self.cell_rect(start);
        let bottom_right = self.cell_rect(end);
        let outline =
            Rect::new(top_left.x0, top_left.y0, bottom_right.x1, bottom_right.y1).intersect(body);
        if !outline.is_zero_area() {
            ctx.stroke_rect(outline, colors.intent.accent_ink, 1.5);
        }
        // The active cell reads strongest.
        let active = self.cell_rect(self.focus).intersect(body);
        if !active.is_zero_area() {
            ctx.stroke_rect(active, colors.intent.accent_ink, 2.0);
        }
        if self.focused {
            ctx.stroke_rect(self.rect.inset(1.0), colors.border_focus, 2.0);
        }
        // A comment's corner mark, unresolved in the accent and resolved quietly.
        for row in self.visible.1.clone() {
            for column in self.visible.0.clone() {
                let addr = (column, row);
                let tone = if self.commented.contains(&addr) {
                    colors.intent.accent_ink
                } else if self.answered.contains(&addr) {
                    colors.text_dim
                } else {
                    continue;
                };
                let rect = self.cell_rect(addr).intersect(body);
                if rect.is_zero_area() {
                    continue;
                }
                let side = f64::from(6);
                let corner = Rect::new(rect.x1 - side, rect.y0, rect.x1, rect.y0 + side);
                ctx.scene.fill(
                    Fill::NonZero,
                    ctx.transform,
                    tone,
                    None,
                    &vello::kurbo::Triangle {
                        a: Point::new(corner.x0, corner.y0),
                        b: Point::new(corner.x1, corner.y0),
                        c: Point::new(corner.x1, corner.y1),
                    },
                );
            }
        }
        // What others chose, in their own ink with their name over it.
        let style = theme.text(TextRole::Caption);
        for ((peer_start, peer_end), who) in &self.peers {
            let a = self.cell_rect(*peer_start);
            let b = self.cell_rect(*peer_end);
            let outline = Rect::new(a.x0, a.y0, b.x1, b.y1).intersect(body);
            if outline.is_zero_area() {
                continue;
            }
            ctx.stroke_rect(outline, colors.intent.you, 1.5);
            let (width, height) = ctx.text.measure_style(who, None, &style);
            let label = Rect::new(
                outline.x0,
                (outline.y0 - f64::from(height)).max(self.rect.y0),
                outline.x0 + f64::from(width) + 8.0,
                (outline.y0).max(self.rect.y0) + 2.0,
            );
            ctx.fill_round(label, 3.0, colors.intent.you);
            ctx.text(
                who,
                &style,
                colors.text_primary,
                Point::new(label.x0 + 4.0, label.y0 + 1.0),
                None,
            );
        }
    }

    /// The scrollbars, quiet until the pointer is near them.
    fn draw_bars(&self, ctx: &mut PaintCtx<'_>, theme: &Theme) {
        let body = self.body();
        let (max_x, max_y) = self.max_scroll();
        if max_y > 0.0 {
            let area = Rect::new(body.x1 - self.metrics.gutter, body.y0, body.x1, body.y1);
            let track = self.bars.0.track_in(area, theme);
            self.bars.0.draw(
                ctx.scene,
                ctx.transform,
                track,
                self.bar_metrics(true),
                theme,
                self.focused,
            );
        }
        if max_x > 0.0 {
            let area = Rect::new(body.x0, body.y1 - self.metrics.gutter, body.x1, body.y1);
            let track = self.bars.1.track_in(area, theme);
            self.bars.1.draw(
                ctx.scene,
                ctx.transform,
                track,
                self.bar_metrics(false),
                theme,
                self.focused,
            );
        }
    }

    /// The fill handle at the selection's corner.
    fn draw_fill_handle(&self, ctx: &mut PaintCtx<'_>, theme: &Theme) {
        if !self.editable {
            return;
        }
        let handle = self.fill_handle_rect().intersect(self.body());
        if handle.is_zero_area() {
            return;
        }
        ctx.fill_round(handle, 2.0, theme.tokens.colors.intent.accent_ink);
    }
}

/// A window's search over the grid: each match is a cell and a span of the text it shows, stepped
/// through in reading order, and a replacement writes the cell through the same edit event an
/// in-cell edit fires. The app's `found` prop marks cells for its own reasons; a search's cells
/// are marked beside them.
impl Findable for Sheet {
    fn find(&mut self, query: &str, options: FindOptions) -> Vec<FindMatch> {
        self.matches.clear();
        self.searched.clear();
        self.dirty = true;
        if query.is_empty() {
            return Vec::new();
        }
        let needle = if options.match_case {
            query.to_owned()
        } else {
            query.to_lowercase()
        };
        let mut addrs: Vec<Addr> = self.cells.keys().copied().collect();
        // Reading order: row by row, left to right.
        addrs.sort_by_key(|(column, row)| (*row, *column));
        for addr in addrs {
            let Some(cell) = self.cells.get(&addr) else {
                continue;
            };
            let hay = if options.match_case {
                cell.text.clone()
            } else {
                cell.text.to_lowercase()
            };
            let mut at = 0;
            while let Some(found) = hay.get(at..).and_then(|rest| rest.find(&needle)) {
                let start = at + found;
                let end = start + needle.len();
                let bounded = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());
                if !options.whole_word
                    || (bounded(hay.get(..start).and_then(|s| s.chars().last()))
                        && bounded(hay.get(end..).and_then(|s| s.chars().next())))
                {
                    self.matches.push((addr, start, end));
                    self.searched.insert(addr);
                }
                at = end;
            }
        }
        self.matches
            .iter()
            .map(|(_, start, end)| FindMatch::new(*start, *end))
            .collect()
    }

    fn reveal(&mut self, index: usize) {
        if let Some((addr, _, _)) = self.matches.get(index).copied() {
            self.reveal(addr);
        }
    }

    fn mark_current(&mut self, index: usize) {
        if let Some((addr, _, _)) = self.matches.get(index).copied() {
            self.focus = addr;
            self.dirty = true;
        }
    }

    fn clear_find(&mut self) {
        if !self.matches.is_empty() || !self.searched.is_empty() {
            self.dirty = true;
        }
        self.matches.clear();
        self.searched.clear();
    }

    fn supports_replace(&self) -> bool {
        self.editable
    }

    fn replace(&mut self, index: usize, with: &str) -> bool {
        if !self.editable {
            return false;
        }
        let Some((addr, start, end)) = self.matches.get(index).copied() else {
            return false;
        };
        let Some(cell) = self.cells.get(&addr) else {
            return false;
        };
        let mut text = cell.text.clone();
        if end > text.len()
            || start > end
            || !text.is_char_boundary(start)
            || !text.is_char_boundary(end)
        {
            return false;
        }
        text.replace_range(start..end, with);
        if let Some(cell) = self.cells.get_mut(&addr) {
            cell.text.clone_from(&text);
        }
        self.revision = self.revision.wrapping_add(1);
        self.fire_edit(vec![(addr, text)]);
        true
    }

    fn replace_all(&mut self, with: &str) -> usize {
        if !self.editable {
            return 0;
        }
        let mut ordered = std::mem::take(&mut self.matches);
        // From the last match of a cell to its first, so each span still names what it matched.
        ordered.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
        let mut writes: Vec<(Addr, String)> = Vec::new();
        for (addr, start, end) in ordered {
            let Some(cell) = self.cells.get(&addr) else {
                continue;
            };
            let mut text = cell.text.clone();
            if end > text.len()
                || start > end
                || !text.is_char_boundary(start)
                || !text.is_char_boundary(end)
            {
                continue;
            }
            text.replace_range(start..end, with);
            if let Some(cell) = self.cells.get_mut(&addr) {
                cell.text.clone_from(&text);
            }
            writes.push((addr, text));
        }
        self.searched.clear();
        let done = writes.len();
        if done > 0 {
            self.revision = self.revision.wrapping_add(1);
            self.fire_edit(writes);
            self.dirty = true;
        }
        done
    }

    fn content_revision(&self) -> u64 {
        self.revision
    }
}

#[cfg(test)]
mod window_tests {
    use super::*;
    use crate::component::{Described, Event, Fired, Intent, Value};
    use crate::widget::InPlace;

    fn cell(reference: &str, text: &str) -> SheetCell {
        SheetCell {
            cell: reference.to_owned(),
            text: text.to_owned(),
            ..SheetCell::default()
        }
    }

    fn sheet_of(queue: &crate::Intents<Fired>, cells: Vec<SheetCell>) -> Sheet {
        let mut sheet = Sheet::new();
        sheet.set_props(crate::widget::SheetProps {
            columns: 4,
            rows: 4,
            cells,
            edit: Event::View(Intent::new(queue, "sheet", "edit")),
            ..Default::default()
        });
        sheet
    }

    /// A window's search reaches a sheet where it stands: it finds the cells in reading order,
    /// shows each match, and a replacement is written through the edit event an in-cell edit
    /// fires.
    #[test]
    fn a_window_s_search_finds_cells_in_reading_order_and_replaces_them() {
        let queue: crate::Intents<Fired> = crate::Intents::default();
        let mut sheet = sheet_of(
            &queue,
            vec![
                cell("A1", "total"),
                cell("B2", "Total spent"),
                cell("A3", "subtotal"),
            ],
        );
        assert_eq!(InPlace::of(&mut sheet), InPlace::Document);

        let found = Findable::find(&mut sheet, "total", FindOptions::default());
        assert_eq!(found.len(), 3, "A1, B2's Total and A3's subtotal");
        assert!(sheet.searched.contains(&(0, 0)));
        assert!(sheet.searched.contains(&(1, 1)));
        assert!(sheet.searched.contains(&(0, 2)));

        assert!(Findable::replace(&mut sheet, 0, "sum"));
        let fired = queue.take();
        assert_eq!(fired.len(), 1, "one edit");
        assert_eq!(fired[0].event, "edit");
        assert_eq!(
            fired[0].value,
            Some(Value::List(vec![
                Value::Text("A1".to_owned()),
                Value::Text("sum".to_owned()),
            ])),
            "the cell's reference and its new text"
        );
        let found = Findable::find(&mut sheet, "total", FindOptions::default());
        assert_eq!(found.len(), 2, "the replaced cell no longer matches");
    }

    /// Options narrow the search as the bar says, and closing it stops marking cells.
    #[test]
    fn match_case_and_whole_word_narrow_and_closing_clears() {
        let queue: crate::Intents<Fired> = crate::Intents::default();
        let mut sheet = sheet_of(&queue, vec![cell("A1", "cat"), cell("A2", "concatenate")]);
        let exact = FindOptions {
            match_case: true,
            ..FindOptions::default()
        };
        assert_eq!(Findable::find(&mut sheet, "cat", exact).len(), 2);
        let word = FindOptions {
            whole_word: true,
            ..FindOptions::default()
        };
        assert_eq!(Findable::find(&mut sheet, "cat", word).len(), 1);
        Findable::clear_find(&mut sheet);
        assert!(sheet.searched.is_empty());
        assert!(sheet.matches.is_empty());
    }

    /// Copy, cut and paste are the person's clipboard, which only the app can reach: the grid
    /// reports the chosen cells as a table and asks the app to paste.
    #[test]
    fn copy_cut_and_paste_are_the_clipboard_events() {
        let queue: crate::Intents<Fired> = crate::Intents::default();
        let mut sheet = Sheet::new();
        sheet.set_props(crate::widget::SheetProps {
            columns: 3,
            rows: 3,
            cells: vec![cell("A1", "a"), cell("B1", "b")],
            selection: Some("A1:B1".to_owned()),
            copy: Event::View(Intent::new(&queue, "sheet", "copy")),
            cut: Event::View(Intent::new(&queue, "sheet", "cut")),
            paste: Event::View(Intent::new(&queue, "sheet", "paste")),
            ..Default::default()
        });
        let ctrl = winit::event::Modifiers::from(winit::keyboard::ModifiersState::CONTROL);
        let press = |ch: &str| {
            crate::synthetic_input::key_event(
                winit::keyboard::Key::Character(ch.into()),
                None,
                winit::event::ElementState::Pressed,
            )
        };
        assert!(sheet.key(&press("c"), ctrl));
        assert!(sheet.key(&press("x"), ctrl));
        assert!(sheet.key(&press("v"), ctrl));
        let fired = queue.take();
        assert_eq!(fired.len(), 3);
        assert_eq!(fired[0].event, "copy");
        assert_eq!(
            fired[0].value,
            Some(Value::Text("a\tb\n".to_owned())),
            "the chosen cells as a table"
        );
        assert_eq!(fired[1].event, "cut");
        assert_eq!(
            fired[1].value,
            Some(Value::Text("a\tb\n".to_owned())),
            "the cells to copy before clearing"
        );
        assert_eq!(fired[2].event, "paste");
        assert_eq!(fired[2].value, None, "the app reads the clipboard itself");
    }
}
