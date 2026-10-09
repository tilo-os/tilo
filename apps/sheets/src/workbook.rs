//! The workbook as stored: its sheets in order, each a [`Grid`] of cells
//! under stable row and column ids, with the charts and comment threads
//! anchored at them.
//!
//! `#[derive(Model)]` is the whole storage contract. A cell's input, format
//! and boldness are field edits at the cell's path
//! (`Workbook::paths().sheets.item(&sheet).cells.cell(&row, &column).raw`),
//! so typing into a cell writes that cell only, and concurrent edits merge
//! per cell. Rows and columns keep their identity when others are inserted
//! or removed: a chart or a comment anchored at a cell stays with it.
//!
//! People and formulas address cells by position (`B3`). A sheet's
//! [`Layout`] maps positions to ids; a sheet holds the rows and columns
//! that were ever written to, and writing past them first adds the missing
//! ones under ids every device derives the same way, so two people
//! reaching the same row concurrently add one row, not two. The verbs in
//! [`crate::resource`] are the operations that are more than writing a
//! field: inserting and deleting rows (which rewrite the formulas pointing
//! past them), sorting, importing, comments.

use std::collections::{BTreeMap, HashMap, HashSet};

use ui::tilo_resource::{self, Atom, Collection, Edit, EditOp, Grid, Id, Model, Path, Refusal};

use crate::model::{Addr, CellFormat, Currency};
use crate::settings::DocSettings;

/// The identity of a new workbook's first sheet. The genesis is the same
/// change on every device, so two people creating the room's workbook before
/// they sync converge on one first sheet rather than two.
pub const FIRST_SHEET_ID: &str = "00000000-0000-4000-8000-000000000001";

/// The longest sheet name.
pub const MAX_SHEET_NAME_CHARS: usize = 100;
/// How many tab colours the palette offers.
pub const TAB_COLOR_COUNT: u32 = 6;
/// Excel-compatible grid bounds.
pub const MAX_COLUMN: u32 = 16_383;
pub const MAX_ROW: u32 = 1_048_575;
/// The longest input one cell takes.
pub const MAX_CELL_INPUT_BYTES: usize = 32 * 1024;
/// The longest comment or reply.
pub const MAX_ANNOTATION_BYTES: usize = 4 * 1024;

/// One cell: what was typed into it, how it shows and whether it is bold.
/// A cell with none of these is not stored.
#[derive(Model, Clone, Debug, Default, PartialEq)]
#[model(check = Cell::valid)]
pub struct Cell {
    /// Input: a value, or a formula starting with `=`. Written whole.
    pub raw: Atom,
    /// Format
    pub format: CellFormat,
    /// Bold
    pub bold: bool,
}

impl Cell {
    /// A cell holding `raw`.
    pub fn of(raw: impl Into<String>) -> Self {
        Self {
            raw: Atom(raw.into()),
            ..Self::default()
        }
    }

    /// Whether the cell holds nothing worth storing.
    pub fn is_blank(&self) -> bool {
        self.raw.0.is_empty() && self.format == CellFormat::General && !self.bold
    }

    fn valid(&self) -> Result<(), Refusal> {
        if self.raw.0.len() > MAX_CELL_INPUT_BYTES {
            return Err(Refusal::new("a cell holds at most 32 KiB"));
        }
        Ok(())
    }
}

/// Which kind of chart.
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

    /// What the chart's picker calls it.
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

/// A rectangle of cells, by the ids of its first and last row and column:
/// it moves with them when rows or columns are inserted around it.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct Area {
    pub top: Id,
    pub left: Id,
    pub bottom: Id,
    pub right: Id,
}

impl Area {
    /// The area of the cells `start` to `end` of a sheet laid out as
    /// `layout`; one naming no cell when the sheet does not reach them.
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

    /// Whether it names no cell.
    #[must_use]
    pub fn is_unset(&self) -> bool {
        [&self.top, &self.left, &self.bottom, &self.right]
            .iter()
            .any(|id| id.as_str().is_empty())
    }
}

/// A chart of a range of cells.
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

/// A reply to a comment thread.
#[derive(Model, Clone, Debug, PartialEq)]
pub struct Reply {
    /// Author
    ///
    /// Who wrote it.
    #[model(readonly)]
    pub author: Atom,
    /// Reply
    pub body: String,
}

/// A comment thread anchored at a range of cells of one sheet.
#[derive(Model, Clone, Debug, PartialEq)]
pub struct Comment {
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
    /// Author
    ///
    /// Who started it.
    #[model(readonly)]
    pub author: Atom,
    /// Comment
    pub body: String,
    /// Resolved
    pub resolved: bool,
    /// Replies
    pub replies: Collection<Reply>,
}

/// One sheet of the workbook.
#[derive(Model, Clone, Debug, PartialEq)]
#[model(check = Sheet::valid)]
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
    /// Cells
    pub cells: Grid<Cell>,
    /// Charts
    pub charts: Collection<Chart>,
}

impl Sheet {
    /// An empty sheet named `name`.
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: Atom(name.into()),
            source: None,
            color: None,
            cells: Grid::new(),
            charts: Collection::new(),
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

    /// Where each row and column of the sheet is.
    pub fn layout(&self) -> Layout {
        Layout::of(&self.cells)
    }

    /// The sheet's cells by position, as the grid shows them.
    pub fn snapshot(&self) -> SheetSnapshot {
        self.snapshot_in(&self.layout())
    }

    /// [`Self::snapshot`] with the layout already at hand.
    pub fn snapshot_in(&self, layout: &Layout) -> SheetSnapshot {
        let mut snapshot = SheetSnapshot {
            name: self.name.0.clone(),
            ..SheetSnapshot::default()
        };
        for (row, column, cell) in self.cells.cells() {
            let Some(addr) = layout.addr(&row, &column) else {
                continue;
            };
            if !cell.raw.0.is_empty() {
                snapshot.raw.insert(addr, cell.raw.0.clone());
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

    /// The cell at `addr`, if it holds anything.
    pub fn cell_at(&self, layout: &Layout, addr: Addr) -> Option<&Cell> {
        let (row, column) = layout.ids(addr)?;
        self.cells.cell(row, column)
    }

    /// Add the rows and columns `addr` needs, under the ids every device
    /// derives for them.
    pub fn reach(&mut self, addr: Addr) {
        let (rows, columns) = Layout::of(&self.cells).missing(addr);
        for id in rows {
            let at = self.cells.rows().len();
            self.cells.insert_row(at, id);
        }
        for id in columns {
            let at = self.cells.columns().len();
            self.cells.insert_column(at, id);
        }
    }

    /// Write `cell` at `addr` (clearing it when blank), adding the rows and
    /// columns it needs.
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

    /// Change the cell at `addr` with `change`, as it is or blank.
    pub fn update(&mut self, addr: Addr, change: impl FnOnce(&mut Cell)) {
        let layout = self.layout();
        let mut cell = self.cell_at(&layout, addr).cloned().unwrap_or_default();
        change(&mut cell);
        self.write(addr, cell);
    }

    /// The area of `start..=end`, adding the rows and columns it needs.
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

    /// The first and last cell of `area`, while its rows and columns exist.
    pub fn corners(layout: &Layout, area: &Area) -> Option<(Addr, Addr)> {
        let start = layout.addr(&area.top, &area.left)?;
        let end = layout.addr(&area.bottom, &area.right)?;
        Some((
            (start.0.min(end.0), start.1.min(end.1)),
            (start.0.max(end.0), start.1.max(end.1)),
        ))
    }

    /// The charts by position, as the sheet shows them.
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

    /// The comment threads of `comments` on sheet `id`, by position, as the
    /// sheet shows them.
    pub fn comment_snapshots(
        id: &Id,
        comments: &Collection<Comment>,
        layout: &Layout,
    ) -> Vec<CommentThread> {
        let mut out: Vec<CommentThread> = comments
            .iter()
            .filter(|(_, comment)| comment.sheet == *id)
            .filter_map(|(id, comment)| {
                let (start, end) = Self::corners(layout, &comment.area)?;
                Some(CommentThread {
                    id: id.as_str().to_owned(),
                    start,
                    end,
                    author: comment.author.0.clone(),
                    body: comment.body.clone(),
                    resolved: comment.resolved,
                    replies: comment
                        .replies
                        .iter()
                        .map(|(id, reply)| CommentReply {
                            id: id.as_str().to_owned(),
                            author: reply.author.0.clone(),
                            body: reply.body.clone(),
                        })
                        .collect(),
                })
            })
            .collect();
        out.sort_by(|a, b| a.start.cmp(&b.start).then_with(|| a.id.cmp(&b.id)));
        out
    }

    /// Whether the sheet holds nothing.
    pub fn is_blank(&self) -> bool {
        self.cells.cells().next().is_none() && self.charts.is_empty()
    }
}

/// Where each row and column of a sheet is: the ids in order, and the
/// position of each.
#[derive(Clone, Debug, Default)]
pub struct Layout {
    rows: Vec<Id>,
    columns: Vec<Id>,
    row_at: HashMap<Id, u32>,
    column_at: HashMap<Id, u32>,
}

impl Layout {
    /// The layout of `grid`.
    pub fn of(grid: &Grid<Cell>) -> Self {
        Self::from_ids(
            grid.rows().cloned().collect(),
            grid.columns().cloned().collect(),
        )
    }

    pub fn rows(&self) -> &[Id] {
        &self.rows
    }

    pub fn columns(&self) -> &[Id] {
        &self.columns
    }

    /// The position of the cell at `row`, `column`.
    pub fn addr(&self, row: &Id, column: &Id) -> Option<Addr> {
        Some((*self.column_at.get(column)?, *self.row_at.get(row)?))
    }

    /// The row and column ids of the cell at `addr`, when the sheet has them.
    pub fn ids(&self, (col, row): Addr) -> Option<(&Id, &Id)> {
        Some((
            self.rows.get(usize::try_from(row).ok()?)?,
            self.columns.get(usize::try_from(col).ok()?)?,
        ))
    }

    /// The rows and columns to add, in order, so the sheet reaches `addr`:
    /// ids derived from what the sheet holds, the same on every device.
    pub fn missing(&self, (col, row): Addr) -> (Vec<Id>, Vec<Id>) {
        (
            extension(&self.rows, row, 'r'),
            extension(&self.columns, col, 'c'),
        )
    }

    /// This layout with what [`Self::missing`] names added.
    #[must_use]
    pub fn extended(&self, addr: Addr) -> Self {
        let (rows, columns) = self.missing(addr);
        let mut rows_now = self.rows.clone();
        rows_now.extend(rows);
        let mut columns_now = self.columns.clone();
        columns_now.extend(columns);
        Self::from_ids(rows_now, columns_now)
    }

    fn from_ids(rows: Vec<Id>, columns: Vec<Id>) -> Self {
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
        }
    }

    /// The edits that add what [`Self::missing`] names to the grid at
    /// `cells`.
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

/// The ids to append to `line` so it has a line at `index`: `r0`, `r1`, …
/// counting on from the highest such id it holds.
fn extension(line: &[Id], index: u32, prefix: char) -> Vec<Id> {
    let Ok(index) = usize::try_from(index) else {
        return Vec::new();
    };
    if index < line.len() {
        return Vec::new();
    }
    let mut next = line
        .iter()
        .filter_map(|id| id.as_str().strip_prefix(prefix)?.parse::<u64>().ok())
        .max()
        .map_or(0, |highest| highest + 1);
    (line.len()..=index)
        .map(|_| {
            let id = Id::from_raw(format!("{prefix}{next}"));
            next += 1;
            id
        })
        .collect()
}

/// The workbook: its sheets in order and its locale overrides.
#[derive(Model, Clone, Debug, PartialEq)]
#[model(check = Workbook::valid)]
pub struct Workbook {
    /// Sheets
    pub sheets: Collection<Sheet>,
    /// Comments
    ///
    /// The comment threads of every sheet: a unit of their own, so who may
    /// only comment writes nothing else.
    #[model(unit)]
    pub comments: Collection<Comment>,
    /// Document settings
    pub settings: DocSettings,
}

impl Default for Workbook {
    /// A new workbook: one empty sheet.
    fn default() -> Self {
        let mut sheets = Collection::new();
        sheets.push(Id::from_raw(FIRST_SHEET_ID), Sheet::named("Sheet1"));
        Self {
            sheets,
            comments: Collection::new(),
            settings: DocSettings::default(),
        }
    }
}

impl Workbook {
    fn valid(&self) -> Result<(), Refusal> {
        if self.sheets.is_empty() {
            return Err(Refusal::new("a workbook keeps at least one sheet"));
        }
        Ok(())
    }

    /// The sheet `id`.
    pub fn sheet(&self, id: &Id) -> Option<&Sheet> {
        self.sheets.get(id)
    }

    /// The id of the sheet at `index`.
    pub fn sheet_id(&self, index: usize) -> Option<&Id> {
        self.sheets.ids().nth(index)
    }

    /// The sheet `id`, to change.
    ///
    /// # Errors
    /// Refuses a sheet the workbook does not have.
    pub fn sheet_mut(&mut self, id: &Id) -> Result<&mut Sheet, Refusal> {
        self.sheets
            .get_mut(id)
            .ok_or_else(|| Refusal::new("no such sheet"))
    }

    /// The next free `SheetN` name.
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

    /// Whether the workbook is still what a new one is: one sheet with
    /// nothing in it.
    pub fn is_blank(&self) -> bool {
        self.sheets.len() == 1
            && self.sheets.values().all(Sheet::is_blank)
            && self.comments.is_empty()
    }

    /// The currency the workbook shows money in when a format names none.
    pub fn currency(&self) -> Currency {
        self.settings.currency.unwrap_or(Currency::USD)
    }
}

/// One sheet's cells by position, for the grid, the formula engine and the
/// file formats.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SheetSnapshot {
    pub name: String,
    /// raw input string per populated cell
    pub raw: HashMap<Addr, String>,
    /// per-cell format (only present when not General)
    pub fmt: HashMap<Addr, CellFormat>,
    /// cells whose text is bold
    pub bold: HashSet<Addr>,
}

/// A chart by position, as the sheet shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChartSnapshot {
    pub id: String,
    pub title: String,
    pub kind: ChartKind,
    pub start: Addr,
    pub end: Addr,
}

/// A reply, as the sheet shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommentReply {
    pub id: String,
    pub author: String,
    pub body: String,
}

/// A comment thread by position, as the sheet shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommentThread {
    pub id: String,
    pub start: Addr,
    pub end: Addr,
    pub author: String,
    pub body: String,
    pub resolved: bool,
    pub replies: Vec<CommentReply>,
}

/// The edits that write `cells` (by position, blank ones clearing) into the
/// sheet `id` of `workbook`: the rows and columns they need first, then each
/// cell at its path. One group of these is one undo step.
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
    for (addr, cell) in written {
        let Some((row, column)) = layout.ids(addr) else {
            continue;
        };
        let stored = sheet.cells.cell(row, column);
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

    #[test]
    fn writing_past_the_sheet_adds_the_same_rows_on_every_device() {
        let mut ours = Sheet::named("S");
        let mut theirs = ours.clone();
        ours.write((1, 2), Cell::of("x"));
        theirs.write((0, 2), Cell::of("y"));
        let rows = |sheet: &Sheet| sheet.layout().rows().to_vec();
        assert_eq!(rows(&ours), rows(&theirs), "the same row ids");
        assert_eq!(
            ours.snapshot().raw.get(&(1, 2)).map(String::as_str),
            Some("x")
        );
        ours.write((1, 2), Cell::default());
        assert!(ours.snapshot().raw.is_empty(), "a blank write clears");
    }

    #[test]
    fn an_area_moves_with_the_rows_around_it() {
        let mut sheet = Sheet::named("S");
        let area = sheet.area((1, 1), (2, 3)).unwrap();
        sheet.cells.insert_row(0, Id::from_raw("new"));
        let layout = sheet.layout();
        assert_eq!(Sheet::corners(&layout, &area), Some(((1, 2), (2, 4))));
    }
}
