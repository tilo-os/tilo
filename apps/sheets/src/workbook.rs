//! The workbook as stored: its sheets in order, each a grid of cells under stable row and column
//! ids, with charts and comment threads anchored at them. A cell's input, format and boldness
//! are field edits at its path, so typing writes that cell only. Positions map to ids through a
//! sheet's [`Layout`], and the ids are derived the same way on every device.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;
use std::sync::OnceLock;

use ui::tilo_resource::{
    self, Atom, Collection, Edit, EditOp, Grid, Id, Model, Path, Refusal, Set,
};

use crate::bound::{Input, Lines};
use crate::model::{Addr, CellFormat};
use crate::settings::DocSettings;
use tilo_locale::Currency;

/// A new workbook's first sheet: the genesis is the same change on every device, so two people
/// converge on one first sheet.
pub const FIRST_SHEET_ID: &str = "00000000-0000-4000-8000-000000000001";

pub const FIRST_SHEET_NAME: &str = "Sheet1";
pub const MAX_TITLE_CHARS: usize = 200;
pub const MAX_SHEET_NAME_CHARS: usize = 100;
pub const TAB_COLOR_COUNT: u32 = 6;
/// The grid's bounds, as Excel's.
pub const MAX_COLUMN: u32 = 16_383;
pub const MAX_ROW: u32 = 1_048_575;
/// The longest input one cell takes.
pub const MAX_CELL_INPUT_BYTES: usize = 32 * 1024;
/// The longest comment or reply.
pub const MAX_ANNOTATION_BYTES: usize = 4 * 1024;

/// One cell: what was typed, its format and boldness; a cell with none is not stored.
#[derive(Model, Clone, Debug, Default, PartialEq)]
#[model(check = Cell::valid)]
pub struct Cell {
    /// The input: a value, or a formula with what its references are bound to, written whole.
    pub raw: Input,
    pub format: CellFormat,
    pub bold: bool,
}

impl Cell {
    pub fn of(raw: impl Into<String>) -> Self {
        Self {
            raw: Input::typed(raw),
            ..Self::default()
        }
    }

    pub fn is_blank(&self) -> bool {
        self.raw.is_empty() && self.format == CellFormat::General && !self.bold
    }

    fn valid(&self) -> Result<(), Refusal> {
        if self.raw.text().len() > MAX_CELL_INPUT_BYTES {
            return Err(Refusal::new("a cell holds at most 32 KiB"));
        }
        Ok(())
    }
}

#[derive(Model, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChartKind {
    #[default]
    Column,
    Line,
}

impl ChartKind {
    pub fn code(self) -> &'static str {
        match self {
            Self::Column => "column",
            Self::Line => "line",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Column => "Column",
            Self::Line => "Line",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "column" => Self::Column,
            "line" => Self::Line,
            _ => return None,
        })
    }
}

impl serde::Serialize for ChartKind {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.code())
    }
}

impl<'de> serde::Deserialize<'de> for ChartKind {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let code = String::deserialize(deserializer)?;
        Self::parse(&code).ok_or_else(|| serde::de::Error::custom("a chart is column or line"))
    }
}

impl tilo_resource::JsonSchema for ChartKind {
    fn json_schema() -> serde_json::Value {
        serde_json::json!({"type": "string", "enum": ["column", "line"]})
    }
}

/// A rectangle of cells by the ids of its first and last row and column, so it moves with them.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct Area {
    pub top: Id,
    pub left: Id,
    pub bottom: Id,
    pub right: Id,
}

impl Area {
    /// The area of `start` to `end` on a sheet laid out this way; none when the sheet does not
    /// reach them.
    #[must_use]
    pub fn of(layout: &Layout, start: Addr, end: Addr) -> Self {
        match (layout.ids(start), layout.ids(end)) {
            (Some((top, left)), Some((bottom, right))) => Self {
                top: top.clone(),
                left: left.clone(),
                bottom: bottom.clone(),
                right: right.clone(),
            },
            _ => Self::default(),
        }
    }

    #[must_use]
    pub fn is_unset(&self) -> bool {
        [&self.top, &self.left, &self.bottom, &self.right]
            .iter()
            .any(|id| id.as_str().is_empty())
    }
}

#[derive(Model, Clone, Debug, PartialEq)]
pub struct Chart {
    /// Title
    #[model(label)]
    pub title: Atom,
    /// Chart type
    pub kind: ChartKind,
    /// Range
    ///
    /// The cells it charts, which move with their rows and columns.
    pub area: Area,
}

#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct CommentCells {
    /// Sheet
    ///
    /// The sheet it is on.
    #[model(readonly)]
    pub sheet: Id,
    /// Range
    ///
    /// The cells it is about.
    #[model(readonly)]
    pub area: Area,
}

pub type Comment = tilo_rich_text::annotation::Comment<CommentCells>;
pub use tilo_rich_text::annotation::Author;

#[derive(Model, Clone, Debug, PartialEq)]
#[model(check = Sheet::valid, check_reads = "name color charts")]
pub struct Sheet {
    /// Name
    #[model(label)]
    pub name: Atom,
    /// Source file
    ///
    /// The file the sheet was imported from, which opening again reveals.
    #[model(readonly)]
    pub source: Option<Atom>,
    /// Tab colour
    ///
    /// An index into the palette, none for the plain tab.
    pub color: Option<u32>,
    pub cells: Grid<Cell>,
    pub charts: Collection<Chart>,
    /// Hidden
    ///
    /// The tab is not shown; its cells and the formulas reading them still
    /// work, as a file that hid the sheet kept them.
    #[model(default = false)]
    pub hidden: bool,
    /// Cut lines
    ///
    /// The rows and columns deleted while they were the last their sheet had
    /// made. A reference to one reads as deleted, not as a line the sheet has
    /// yet to reach, and no line made later takes its id.
    #[model(readonly)]
    pub cut: Set<Id>,
}

impl Sheet {
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: Atom(name.into()),
            source: None,
            color: None,
            cells: Grid::new(),
            charts: Collection::new(),
            hidden: false,
            cut: Set::new(),
        }
    }

    fn valid(&self) -> Result<(), Refusal> {
        let name = self.name.0.trim();
        if name.is_empty() {
            return Err(Refusal::new("a sheet needs a name"));
        }
        if name.chars().count() > MAX_SHEET_NAME_CHARS {
            return Err(Refusal::new("a sheet name has at most 100 characters"));
        }
        if self.color.is_some_and(|color| color >= TAB_COLOR_COUNT) {
            return Err(Refusal::new("no such tab colour"));
        }
        if self.charts.values().any(|chart| chart.area.is_unset()) {
            return Err(Refusal::new("a chart charts cells of its sheet"));
        }
        Ok(())
    }

    pub fn layout(&self) -> Layout {
        Layout::of(&self.cells, &self.cut)
    }

    /// The sheet's cells by position, laid out as `layout`: each input as it reads now
    /// ([`Input::shown`]), the other sheets found through `lines`.
    pub fn snapshot_in(&self, layout: &Layout, lines: &Lines<'_>) -> SheetSnapshot {
        let mut snapshot = SheetSnapshot {
            name: self.name.0.clone(),
            hidden: self.hidden,
            ..SheetSnapshot::default()
        };
        for (row, column, cell) in self.cells.cells() {
            let Some(addr) = layout.addr(&row, &column) else {
                continue;
            };
            if !cell.raw.is_empty() {
                snapshot
                    .raw
                    .insert(addr, cell.raw.shown(layout, lines).into_owned());
            }
            if cell.format != CellFormat::General {
                snapshot.fmt.insert(addr, cell.format);
            }
            if cell.bold {
                snapshot.bold.insert(addr);
            }
        }
        snapshot
    }

    /// Every input of the sheet by position, as it reads now: what [`Self::snapshot_in`]
    /// holds as `raw`.
    pub fn inputs_in(&self, layout: &Layout, lines: &Lines<'_>) -> HashMap<Addr, String> {
        self.cells
            .cells()
            .filter(|(_, _, cell)| !cell.raw.is_empty())
            .filter_map(|(row, column, cell)| {
                Some((
                    layout.addr(&row, &column)?,
                    cell.raw.shown(layout, lines).into_owned(),
                ))
            })
            .collect()
    }

    pub fn cell_at(&self, layout: &Layout, addr: Addr) -> Option<&Cell> {
        let (row, column) = layout.ids(addr)?;
        self.cells.cell(row, column)
    }

    /// Add the rows and columns `addr` needs, under the ids every device derives.
    pub fn reach(&mut self, addr: Addr) {
        let (rows, columns) = self.layout().missing(addr);
        for id in rows {
            let at = self.cells.rows().len();
            self.cells.insert_row(at, id);
        }
        for id in columns {
            let at = self.cells.columns().len();
            self.cells.insert_column(at, id);
        }
    }

    /// Write `cell` at `addr`, clearing it when blank, and add what it needs.
    pub fn write(&mut self, addr: Addr, cell: Cell) {
        if cell.is_blank() {
            let layout = self.layout();
            if let Some((row, column)) = layout.ids(addr) {
                self.cells.set_cell(row, column, None);
            }
            return;
        }
        self.reach(addr);
        let layout = self.layout();
        if let Some((row, column)) = layout.ids(addr) {
            self.cells.set_cell(row, column, Some(cell));
        }
    }

    /// Change the cell at `addr` with `change`.
    pub fn update(&mut self, addr: Addr, change: impl FnOnce(&mut Cell)) {
        let layout = self.layout();
        let mut cell = self.cell_at(&layout, addr).cloned().unwrap_or_default();
        change(&mut cell);
        self.write(addr, cell);
    }

    /// The area of `start..=end`, adding what it needs.
    pub fn area(&mut self, start: Addr, end: Addr) -> Option<Area> {
        self.reach(end);
        let layout = self.layout();
        let (top, left) = layout.ids(start)?;
        let (bottom, right) = layout.ids(end)?;
        Some(Area {
            top: top.clone(),
            left: left.clone(),
            bottom: bottom.clone(),
            right: right.clone(),
        })
    }

    pub fn corners(layout: &Layout, area: &Area) -> Option<(Addr, Addr)> {
        let start = layout.addr(&area.top, &area.left)?;
        let end = layout.addr(&area.bottom, &area.right)?;
        Some((
            (start.0.min(end.0), start.1.min(end.1)),
            (start.0.max(end.0), start.1.max(end.1)),
        ))
    }

    pub fn chart_snapshots(&self, layout: &Layout) -> Vec<ChartSnapshot> {
        let mut out: Vec<ChartSnapshot> = self
            .charts
            .iter()
            .filter_map(|(id, chart)| {
                let (start, end) = Self::corners(layout, &chart.area)?;
                Some(ChartSnapshot {
                    id: id.as_str().to_owned(),
                    title: chart.title.0.clone(),
                    kind: chart.kind,
                    start,
                    end,
                })
            })
            .collect();
        out.sort_by(|a, b| a.title.cmp(&b.title).then_with(|| a.id.cmp(&b.id)));
        out
    }

    /// The comment threads of `comments` on the sheet, by position.
    pub fn comment_snapshots(
        id: &Id,
        comments: &Collection<Comment>,
        layout: &Layout,
    ) -> Vec<CommentThread> {
        let mut out: Vec<CommentThread> = comments
            .iter()
            .filter(|(_, comment)| comment.anchor.sheet == *id)
            .filter_map(|(id, comment)| {
                let (start, end) = Self::corners(layout, &comment.anchor.area)?;
                Some(CommentThread {
                    id: id.as_str().to_owned(),
                    start,
                    end,
                    author: comment.author.clone(),
                    body: comment.text.clone(),
                    resolved: comment.resolved,
                    replies: comment
                        .replies
                        .iter()
                        .map(|(id, reply)| CommentReply {
                            id: id.as_str().to_owned(),
                            author: reply.author.clone(),
                            body: reply.text.clone(),
                        })
                        .collect(),
                })
            })
            .collect();
        out.sort_by(|a, b| a.start.cmp(&b.start).then_with(|| a.id.cmp(&b.id)));
        out
    }

    pub fn is_blank(&self) -> bool {
        self.cells.cells().next().is_none() && self.charts.is_empty()
    }

    /// Whether it is still the sheet a new workbook starts with. A blank sheet someone named is
    /// one they mean to fill.
    pub fn is_untouched(&self) -> bool {
        self.is_blank()
            && self.name.0 == FIRST_SHEET_NAME
            && self.color.is_none()
            && self.source.is_none()
            && !self.hidden
    }
}

/// Where each row and column of a sheet is: the ids in order, and each one's position.
#[derive(Clone, Debug, Default)]
pub struct Layout {
    rows: Vec<Id>,
    columns: Vec<Id>,
    row_at: HashMap<Id, u32>,
    column_at: HashMap<Id, u32>,
    /// The highest number among the derived rows, and among the derived columns, cut from the
    /// sheet's end.
    cut: (Option<u32>, Option<u32>),
    /// What the ids say of the sheet, read when first asked.
    made: OnceLock<Made>,
}

/// How far a sheet has numbered its own rows and columns, and whether those are all it holds.
#[derive(Clone, Copy, Debug)]
struct Made {
    /// One past the highest number a derived row ever had here; the next row the sheet makes
    /// takes it.
    rows: u32,
    columns: u32,
    /// Every line is the one the sheet derives for its place: nothing was inserted, and
    /// nothing deleted but from the end.
    plain: bool,
    /// The rows and columns in their order, and what was cut, as one number
    /// ([`Layout::stamp`]).
    stamp: u64,
}

/// FNV-1a over what sheets and their lines are called. It is the same on every device and in
/// every build, which a hasher of the standard library is not promised to be.
pub(crate) struct Fnv(u64);

impl Fnv {
    pub(crate) fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    fn byte(&mut self, byte: u8) {
        self.0 = (self.0 ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
    }

    /// `text`, and where it ends.
    pub(crate) fn text(&mut self, text: &str) {
        for byte in text.bytes().chain([0xff]) {
            self.byte(byte);
        }
    }

    pub(crate) fn number(&mut self, number: u64) {
        for byte in number.to_le_bytes() {
            self.byte(byte);
        }
    }

    pub(crate) fn finish(&self) -> u64 {
        self.0
    }
}

/// The id a sheet derives for the row (`r`) or column (`c`) it makes as its `number`th.
pub(crate) fn derived_id(prefix: char, number: u32) -> String {
    format!("{prefix}{number}")
}

/// The number of a derived id: `r12` is row number 12. `None` for an id made otherwise.
fn derived_number(id: &str, prefix: char) -> Option<u32> {
    let digits = id.strip_prefix(prefix)?;
    if digits.is_empty() || (digits.len() > 1 && digits.starts_with('0')) {
        return None;
    }
    if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

impl Layout {
    pub fn of(grid: &Grid<Cell>, cut: &Set<Id>) -> Self {
        let highest = |prefix: char| {
            cut.iter()
                .filter_map(|id| derived_number(id.as_str(), prefix))
                .max()
        };
        Self::from_ids(
            grid.rows().cloned().collect(),
            grid.columns().cloned().collect(),
            (highest('r'), highest('c')),
        )
    }

    fn made(&self) -> Made {
        *self.made.get_or_init(|| {
            let mut stamp = Fnv::new();
            let mut read = |line: &[Id], prefix: char, cut: Option<u32>| {
                let (mut highest, mut ordered) = (cut, true);
                for (at, id) in (0u32..).zip(line) {
                    stamp.text(id.as_str());
                    match derived_number(id.as_str(), prefix) {
                        Some(number) => {
                            highest = Some(highest.map_or(number, |held| held.max(number)));
                            ordered &= number == at;
                        }
                        None => ordered = false,
                    }
                }
                let made = highest.map_or(0, |highest| highest.saturating_add(1));
                stamp.number(u64::from(made));
                (made, ordered)
            };
            let (rows, rows_ordered) = read(&self.rows, 'r', self.cut.0);
            let (columns, columns_ordered) = read(&self.columns, 'c', self.cut.1);
            Made {
                rows,
                columns,
                plain: rows_ordered
                    && columns_ordered
                    && usize::try_from(rows).is_ok_and(|rows| rows == self.rows.len())
                    && usize::try_from(columns).is_ok_and(|columns| columns == self.columns.len()),
                stamp: stamp.finish(),
            }
        })
    }

    /// The rows and columns in their order, with how far the sheet has numbered its own, as
    /// one number: a binding finds the same lines in two layouts of one stamp.
    pub fn stamp(&self) -> u64 {
        self.made().stamp
    }

    /// Whether every row and column is the one the sheet derives for its place, so a line's
    /// number is where it is.
    pub fn is_plain(&self) -> bool {
        self.made().plain
    }

    /// Whether row `id` is the last the sheet has numbered: deleting it must be remembered
    /// ([`Sheet::cut`]), or the next row made would take its id.
    pub fn is_last_row(&self, id: &Id) -> bool {
        derived_number(id.as_str(), 'r')
            .is_some_and(|number| number.saturating_add(1) == self.made().rows)
    }

    /// [`Self::is_last_row`] for a column.
    pub fn is_last_column(&self, id: &Id) -> bool {
        derived_number(id.as_str(), 'c')
            .is_some_and(|number| number.saturating_add(1) == self.made().columns)
    }

    /// Where the row a binding names is: by `code`, its id, or with none the row the sheet
    /// derives for position `written`. A row the sheet has yet to make comes after every row
    /// it holds; `None` for a row that was deleted.
    pub fn row_of(&self, code: &str, written: u32) -> Option<u32> {
        self.line_of(true, code, written)
    }

    /// [`Self::row_of`] for a column.
    pub fn column_of(&self, code: &str, written: u32) -> Option<u32> {
        self.line_of(false, code, written)
    }

    fn line_of(&self, rows: bool, code: &str, written: u32) -> Option<u32> {
        let (line, at, prefix, made) = if rows {
            (&self.rows, &self.row_at, 'r', self.made().rows)
        } else {
            (&self.columns, &self.column_at, 'c', self.made().columns)
        };
        let derived = if code.is_empty() {
            Some(written)
        } else {
            derived_number(code, prefix)
        };
        let Some(number) = derived else {
            return at.get(&Id::from_raw(code)).copied();
        };
        // Most lines are where their number says.
        let there = usize::try_from(number).ok().and_then(|at| line.get(at));
        if there.is_some_and(|id| derived_number(id.as_str(), prefix) == Some(number)) {
            return Some(number);
        }
        if let Some(place) = at.get(&Id::from_raw(derived_id(prefix, number))) {
            return Some(*place);
        }
        let held = u32::try_from(line.len()).ok()?;
        (number >= made).then(|| held.saturating_add(number - made))
    }

    /// How a binding names the row at position `at` for a reference whose text names position
    /// `written`: nothing when it is the row the sheet derives for `written`, else its id; for
    /// a row the sheet has yet to make, the id it will derive for it. `None` outside the grid
    /// and for an id a binding cannot hold.
    pub fn row_code(&self, at: u32, written: u32) -> Option<String> {
        self.line_code(true, at, written)
    }

    /// [`Self::row_code`] for a column.
    pub fn column_code(&self, at: u32, written: u32) -> Option<String> {
        self.line_code(false, at, written)
    }

    fn line_code(&self, rows: bool, at: u32, written: u32) -> Option<String> {
        let (line, prefix, made, most) = if rows {
            (&self.rows, 'r', self.made().rows, MAX_ROW)
        } else {
            (&self.columns, 'c', self.made().columns, MAX_COLUMN)
        };
        if at > most {
            return None;
        }
        let held = u32::try_from(line.len()).ok()?;
        let number = match usize::try_from(at).ok().and_then(|at| line.get(at)) {
            Some(id) => {
                let id = id.as_str();
                let Some(number) = derived_number(id, prefix) else {
                    let plain = id
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
                    return (plain && !id.is_empty()).then(|| id.to_owned());
                };
                number
            }
            None => made.saturating_add(at - held),
        };
        Some(if number == written {
            String::new()
        } else {
            derived_id(prefix, number)
        })
    }

    pub fn rows(&self) -> &[Id] {
        &self.rows
    }

    pub fn columns(&self) -> &[Id] {
        &self.columns
    }

    pub fn addr(&self, row: &Id, column: &Id) -> Option<Addr> {
        Some((*self.column_at.get(column)?, *self.row_at.get(row)?))
    }

    pub fn ids(&self, (col, row): Addr) -> Option<(&Id, &Id)> {
        Some((
            self.rows.get(usize::try_from(row).ok()?)?,
            self.columns.get(usize::try_from(col).ok()?)?,
        ))
    }

    /// The rows and columns to add so the sheet reaches `addr`, under ids derived from what it
    /// holds and what was cut from its end.
    pub fn missing(&self, (col, row): Addr) -> (Vec<Id>, Vec<Id>) {
        let made = self.made();
        (
            extension(&self.rows, row, 'r', made.rows),
            extension(&self.columns, col, 'c', made.columns),
        )
    }

    #[must_use]
    pub fn extended(&self, addr: Addr) -> Self {
        let (rows, columns) = self.missing(addr);
        let mut rows_now = self.rows.clone();
        rows_now.extend(rows);
        let mut columns_now = self.columns.clone();
        columns_now.extend(columns);
        Self::from_ids(rows_now, columns_now, self.cut)
    }

    fn from_ids(rows: Vec<Id>, columns: Vec<Id>, cut: (Option<u32>, Option<u32>)) -> Self {
        let index = |ids: &[Id]| {
            ids.iter()
                .zip(0u32..)
                .map(|(id, at)| (id.clone(), at))
                .collect::<HashMap<_, _>>()
        };
        Self {
            row_at: index(&rows),
            column_at: index(&columns),
            rows,
            columns,
            cut,
            made: OnceLock::new(),
        }
    }

    /// The edits that add what is missing to the grid.
    pub fn reaching<R>(&self, cells: &Path<R, Grid<Cell>>, addr: Addr) -> Vec<Edit<R>> {
        let (rows, columns) = self.missing(addr);
        let mut edits = Vec::new();
        for (at, id) in (self.rows.len()..).zip(rows) {
            edits.push(cells.edit(EditOp::InsertRow { id: Some(id), at }));
        }
        for (at, id) in (self.columns.len()..).zip(columns) {
            edits.push(cells.edit(EditOp::InsertColumn { id: Some(id), at }));
        }
        edits
    }
}

/// The ids to append so the line reaches `index`, counting on from `made`: one past the highest
/// number a derived line of it ever had.
fn extension(line: &[Id], index: u32, prefix: char, made: u32) -> Vec<Id> {
    let Ok(index) = usize::try_from(index) else {
        return Vec::new();
    };
    if index < line.len() {
        return Vec::new();
    }
    (line.len()..=index)
        .zip(made..)
        .map(|(_, number)| Id::from_raw(derived_id(prefix, number)))
        .collect()
}

/// The workbook: its sheets in order and its locale overrides.
#[derive(Model, Clone, Debug, PartialEq)]
#[model(check = Workbook::valid, check_items = "sheets")]
pub struct Workbook {
    pub sheets: Collection<Sheet>,
    /// Comments
    ///
    /// The comment threads of every sheet: a unit of their own, so who may
    /// only comment writes nothing else.
    #[model(unit)]
    pub comments: Collection<Comment>,
    pub settings: DocSettings,
    /// Title
    ///
    /// What people call the spreadsheet; its first shown sheet names it
    /// while it has none.
    #[model(default = None)]
    pub title: Option<String>,
}

impl Default for Workbook {
    /// A new workbook: one empty sheet.
    fn default() -> Self {
        let mut sheets = Collection::new();
        sheets.push(Id::from_raw(FIRST_SHEET_ID), Sheet::named(FIRST_SHEET_NAME));
        Self {
            sheets,
            comments: Collection::new(),
            settings: DocSettings::default(),
            title: None,
        }
    }
}

const NAMED_SHEETS_LISTED: usize = 20;

impl Workbook {
    fn valid(&self) -> Result<(), Refusal> {
        if self.sheets.is_empty() {
            return Err(Refusal::new("a workbook keeps at least one sheet"));
        }
        if self.sheets.values().all(|sheet| sheet.hidden) {
            return Err(Refusal::new("a workbook keeps at least one sheet shown"));
        }
        if let Some(title) = &self.title
            && (title.trim().is_empty() || title.chars().count() > MAX_TITLE_CHARS)
        {
            return Err(Refusal::new("a workbook title has 1 to 200 characters"));
        }
        Ok(())
    }

    /// The id of the sheet `key` names: its id, its item's resource id, or its name, case and
    /// surrounding space ignored.
    ///
    /// # Errors
    /// Refuses a key that names no sheet, listing the sheets.
    pub fn sheet_key(&self, key: &Id) -> Result<Id, Refusal> {
        if self.sheets.get(key).is_some() {
            return Ok(key.clone());
        }
        let text = key.as_str().trim();
        if let Some((_, id)) = tilo_resource::items::item_of_resource::<Self>(text)
            && self.sheets.get(&id).is_some()
        {
            return Ok(id);
        }
        let named = |exact: bool| {
            self.sheets.iter().find(|(_, sheet)| {
                if exact {
                    sheet.name.0 == text
                } else {
                    sheet.name.0.trim().to_lowercase() == text.to_lowercase()
                }
            })
        };
        if let Some((id, _)) = named(true).or_else(|| named(false)) {
            return Ok(id.clone());
        }
        let mut listed: Vec<String> = self
            .sheets
            .values()
            .take(NAMED_SHEETS_LISTED)
            .map(|sheet| format!("`{}`", sheet.name.0))
            .collect();
        if self.sheets.len() > NAMED_SHEETS_LISTED {
            listed.push(format!(
                "and {} more",
                self.sheets.len() - NAMED_SHEETS_LISTED
            ));
        }
        Err(Refusal::new(format!(
            "no sheet `{text}`; the sheets are {}",
            listed.join(", ")
        )))
    }

    /// The cells of sheet `id` by position, each formula as it reads now, with the other sheets
    /// when its formulas read them.
    pub fn snapshot_of(&self, id: &Id) -> Option<SheetSnapshot> {
        self.snapshot_among(id, &Lines::of(self))
    }

    /// [`Self::snapshot_of`], the sheets found through `lines`, which may hold layouts its
    /// caller already made.
    pub fn snapshot_among(&self, id: &Id, lines: &Lines<'_>) -> Option<SheetSnapshot> {
        let sheet = self.sheet(id)?;
        let mut snap = sheet.snapshot_in(&*lines.layout(id)?, lines);
        if crate::calc::Book::needed(&snap.raw) {
            let sheets = self
                .sheets
                .iter()
                .map(|(other, sheet)| {
                    let raw = if other == id {
                        snap.raw.clone()
                    } else {
                        lines
                            .layout(other)
                            .map(|layout| sheet.inputs_in(&layout, lines))
                            .unwrap_or_default()
                    };
                    (sheet.name.0.clone(), raw)
                })
                .collect();
            let own = self.sheets.position(id).unwrap_or_default();
            snap.book = Some(std::sync::Arc::new(crate::calc::Book { sheets, own }));
        }
        Some(snap)
    }

    pub fn sheet(&self, id: &Id) -> Option<&Sheet> {
        self.sheets.get(id)
    }

    /// The name formulas reach sheet `id` by, folded as [`crate::refs::fold_sheet`] does. A
    /// reference takes the first sheet of its name, so a later sheet of the same name has none.
    pub fn name_reaching(&self, id: &Id) -> Option<String> {
        let name = crate::refs::fold_sheet(&self.sheet(id)?.name.0);
        self.sheets
            .iter()
            .find(|(_, other)| crate::refs::fold_sheet(&other.name.0) == name)
            .is_some_and(|(first, _)| first == id)
            .then_some(name)
    }

    pub fn sheet_id(&self, index: usize) -> Option<&Id> {
        self.sheets.ids().nth(index)
    }

    pub fn sheet_mut(&mut self, id: &Id) -> Result<&mut Sheet, Refusal> {
        self.sheets
            .get_mut(id)
            .ok_or_else(|| Refusal::new("no such sheet"))
    }

    pub fn next_sheet_name(&self) -> String {
        let taken: HashSet<&str> = self
            .sheets
            .values()
            .map(|sheet| sheet.name.0.as_str())
            .collect();
        // One of the next `len + 1` numbers is free.
        (self.sheets.len() + 1..=2 * self.sheets.len() + 1)
            .map(|n| format!("Sheet{n}"))
            .find(|name| !taken.contains(name.as_str()))
            .unwrap_or_else(|| "Sheet".to_owned())
    }

    /// Whether the workbook is still what a new one is. An import replaces it rather than leaving an
    /// empty first tab; a renamed blank sheet is kept.
    pub fn is_blank(&self) -> bool {
        self.sheets.len() == 1
            && self.sheets.values().all(Sheet::is_untouched)
            && self.comments.is_empty()
    }

    pub fn currency(&self) -> Currency {
        self.settings.currency.unwrap_or(Currency::USD)
    }

    /// The sheets and where their rows and columns are, as a short stamp. A read answers it as
    /// `seen`; a write that names cells by position can hand it back, and is refused when a
    /// row, a column or a sheet has moved since, instead of landing on other cells than its
    /// caller read.
    pub fn seen(&self) -> String {
        let mut stamp = Fnv::new();
        for (id, sheet) in self.sheets.iter() {
            stamp.text(id.as_str());
            stamp.text(&sheet.name.0);
            for row in sheet.cells.rows() {
                stamp.text(row.as_str());
            }
            stamp.text("");
            for column in sheet.cells.columns() {
                stamp.text(column.as_str());
            }
            stamp.text("");
        }
        let hash = stamp.finish();
        ui::format::machine!(
            "a stamp a caller hands back as it got it, not a number shown",
            format!("{hash:016x}")
        )
    }
}

/// One sheet's cells by position.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SheetSnapshot {
    pub name: String,
    pub raw: HashMap<Addr, String>,
    pub fmt: HashMap<Addr, CellFormat>,
    pub bold: HashSet<Addr>,
    pub hidden: bool,
    /// The workbook's sheets, when this one's formulas read them.
    pub book: Option<std::sync::Arc<crate::calc::Book>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChartSnapshot {
    pub id: String,
    pub title: String,
    pub kind: ChartKind,
    pub start: Addr,
    pub end: Addr,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommentReply {
    pub id: String,
    pub author: Author,
    pub body: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommentThread {
    pub id: String,
    pub start: Addr,
    pub end: Addr,
    pub author: Author,
    pub body: String,
    pub resolved: bool,
    pub replies: Vec<CommentReply>,
}

/// `cell` as it is stored when written over `stored`. A formula typed anew is bound to the lines
/// it names now; one that reads as the stored one does is the stored one, so entering a formula
/// unchanged writes nothing and keeps what it was bound to.
fn entered<'c>(
    cell: &'c Cell,
    stored: Option<&Cell>,
    layout: &Layout,
    lines: &Lines<'_>,
) -> Cow<'c, Cell> {
    if cell.raw.is_bound() || !cell.raw.is_formula() {
        return Cow::Borrowed(cell);
    }
    let raw = match stored {
        Some(stored) if stored.raw.shown(layout, lines) == cell.raw.text() => stored.raw.clone(),
        _ => Input::bound(cell.raw.text(), layout, lines),
    };
    Cow::Owned(Cell {
        raw,
        ..cell.clone()
    })
}

/// The edits that write `cells` into the sheet, the rows and columns they need first; one group
/// is one undo step. A formula among them is bound to the rows, columns and sheets it names as
/// the workbook holds them now.
pub fn cell_edits(workbook: &Workbook, id: &Id, cells: &[(Addr, Cell)]) -> Vec<Edit<Workbook>> {
    let Some(sheet) = workbook.sheet(id) else {
        return Vec::new();
    };
    let path = Workbook::paths().sheets.item(id).cells;
    let mut layout = sheet.layout();
    let mut edits = Vec::new();
    if let Some(far) = cells
        .iter()
        .filter(|(_, cell)| !cell.is_blank())
        .map(|(addr, _)| *addr)
        .reduce(|far, (col, row)| (far.0.max(col), far.1.max(row)))
    {
        edits.extend(layout.reaching(&path, far));
        layout = layout.extended(far);
    }
    let mut written = BTreeMap::new();
    for (addr, cell) in cells {
        written.insert(*addr, cell);
    }
    let layout = Rc::new(layout);
    let lines = Lines::with(workbook, id, Rc::clone(&layout));
    for (addr, cell) in written {
        let Some((row, column)) = layout.ids(addr) else {
            continue;
        };
        let stored = sheet.cells.cell(row, column);
        let cell = entered(cell, stored, &layout, &lines);
        let cell = &*cell;
        if stored == Some(cell) || (stored.is_none() && cell.is_blank()) {
            continue;
        }
        edits.push(path.set_cell(row, column, (!cell.is_blank()).then_some(cell)));
    }
    edits
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn a_sheets_rows_and_areas_hold_their_place;

        phase writing_past_the_sheet_adds_the_same_rows_on_every_device;
        {
            let mut ours = Sheet::named("S");
            let mut theirs = ours.clone();
            ours.write((1, 2), Cell::of("x"));
            theirs.write((0, 2), Cell::of("y"));
            let rows = |sheet: &Sheet| sheet.layout().rows().to_vec();
            assert_eq!(rows(&ours), rows(&theirs), "the same row ids");
            let held = |sheet: &Sheet| {
                sheet
                    .cell_at(&sheet.layout(), (1, 2))
                    .map(|cell| cell.raw.text().to_owned())
            };
            assert_eq!(held(&ours).as_deref(), Some("x"));
            ours.write((1, 2), Cell::default());
            assert_eq!(held(&ours), None, "a blank write clears");
        }

        phase an_area_moves_with_the_rows_around_it;
        {
            let mut sheet = Sheet::named("S");
            let area = sheet.area((1, 1), (2, 3)).unwrap();
            sheet.cells.insert_row(0, Id::from_raw("new"));
            let layout = sheet.layout();
            assert_eq!(Sheet::corners(&layout, &area), Some(((1, 2), (2, 4))));
        }
    }
}
