//! The workbook's verbs, the places derived from it, and the app.
//!
//! Plain fields change through the built-in field edits every path offers:
//! a cell's input, format and boldness, a sheet's name and tab colour, its
//! place among the sheets, a chart's title and type, a comment's text and
//! whether it is resolved, the workbook's locale overrides. The view types
//! into cells that way ([`crate::workbook::cell_edits`]). The methods below
//! are the operations that are more than setting a field: they address
//! cells by position (`B3`, `A2:D9`), as people and agents do, and keep
//! formulas, charts and comments pointing at the cells they meant when rows
//! and columns move.

use std::rc::Rc;

use serde::{Deserialize, Serialize};
use ui::framework::{
    EffectAnswer, EffectCx, Fixture, ResourceApp, ResourceSurface, SurfaceCx, ViewCx,
};
use ui::i18n::EmbeddedCatalog;
use ui::tilo_resource::{
    self, ActionCx, ActionDecl, Atom, Derived, Id, Invoke, JsonSchema, Refusal, SearchEntry,
    resource,
};
use ui::widget::Element;

use crate::actions::SheetsEffect;
use crate::app::SheetsApp;
use crate::model::{Addr, CellFormat, GridEdit, a1_to_addr, addr_to_a1, letters_to_col};
use crate::workbook::{
    Area, Chart, ChartKind, Comment, Layout, MAX_ANNOTATION_BYTES, MAX_COLUMN, MAX_ROW, Reply,
    Sheet, Workbook,
};

/// Sheets' Fluent catalogs, source locale first.
pub const LOCALES: &[(&str, &str)] = &[
    ("en-US", include_str!("../locales/en-US.ftl")),
    ("de-DE", include_str!("../locales/de-DE.ftl")),
];

static CATALOG: EmbeddedCatalog = EmbeddedCatalog::new("en-US", LOCALES).with_system_locale();

/// The most cells one write, sort or import changes.
pub const MAX_WRITES: usize = 16 * 1024;
/// The most cells one import brings, over all its sheets.
pub const MAX_IMPORTED_CELLS: usize = 64 * 1024;
/// The most sheets one import brings.
pub const MAX_IMPORTED_SHEETS: usize = 64;

/// One cell a write changes, by its reference. What it leaves out stays as
/// it is: an input of `""` clears the cell's value.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CellWrite {
    /// The cell, e.g. `B3`.
    pub cell: String,
    /// What to type into it: a value, or a formula starting with `=`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    /// How it shows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<CellFormat>,
    /// Bold text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bold: Option<bool>,
}

impl CellWrite {
    /// The write of `input` (and `format`, when given) at `addr`.
    pub fn at(addr: Addr, input: impl Into<String>, format: Option<CellFormat>) -> Self {
        Self {
            cell: addr_to_a1(addr.0, addr.1),
            input: Some(input.into()),
            format,
            bold: None,
        }
    }
}

/// One sheet an import brings: its name and its cells.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ImportedSheet {
    pub name: String,
    pub cells: Vec<CellWrite>,
}

fn cell_of(reference: &str) -> Result<Addr, Refusal> {
    a1_to_addr(reference.trim())
        .filter(|(col, row)| *col <= MAX_COLUMN && *row <= MAX_ROW)
        .ok_or_else(|| Refusal::new(format!("`{reference}` is not a cell such as B3")))
}

/// `A1` or `A1:D6`, normalised so the first corner is the top-left one.
pub fn parse_range(text: &str) -> Option<(Addr, Addr)> {
    let (a, b) = text.split_once(':').unwrap_or((text, text));
    let (a, b) = (a1_to_addr(a.trim())?, a1_to_addr(b.trim())?);
    let range = ((a.0.min(b.0), a.1.min(b.1)), (a.0.max(b.0), a.1.max(b.1)));
    (range.1.0 <= MAX_COLUMN && range.1.1 <= MAX_ROW).then_some(range)
}

/// Cells as people write them: `B3`, or `B3:D9`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellRange {
    pub start: Addr,
    pub end: Addr,
}

impl serde::Serialize for CellRange {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&range_label(self.start, self.end))
    }
}

impl<'de> serde::Deserialize<'de> for CellRange {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        let (start, end) = parse_range(&text).ok_or_else(|| {
            serde::de::Error::custom(format!("`{text}` is not a cell or range such as A1:D6"))
        })?;
        Ok(Self { start, end })
    }
}

impl tilo_resource::JsonSchema for CellRange {
    fn json_schema() -> serde_json::Value {
        serde_json::json!({
            "type": "string",
            "pattern": "^[A-Za-z]+[0-9]+(:[A-Za-z]+[0-9]+)?$",
            "description": "A cell or a range of cells, e.g. B3 or B2:D6"
        })
    }
}

fn range_of(text: &str) -> Result<(Addr, Addr), Refusal> {
    parse_range(text)
        .ok_or_else(|| Refusal::new(format!("`{text}` is not a cell or range such as A1:D6")))
}

/// `start..=end` as people write it: `B3`, or `B3:D9`.
pub fn range_label(start: Addr, end: Addr) -> String {
    if start == end {
        addr_to_a1(start.0, start.1)
    } else {
        format!(
            "{}:{}",
            addr_to_a1(start.0, start.1),
            addr_to_a1(end.0, end.1)
        )
    }
}

fn annotation(text: &str) -> Result<String, Refusal> {
    let text = text.trim();
    if text.is_empty() {
        return Err(Refusal::new("write something"));
    }
    if text.len() > MAX_ANNOTATION_BYTES {
        return Err(Refusal::new("at most 4 KiB"));
    }
    Ok(text.to_owned())
}

/// Write `cells` into `sheet`, each changing only what it names.
fn write_into(sheet: &mut Sheet, cells: &[CellWrite]) -> Result<(), Refusal> {
    if cells.len() > MAX_WRITES {
        return Err(Refusal::new(format!("at most {MAX_WRITES} cells at once")));
    }
    let addrs = cells
        .iter()
        .map(|write| cell_of(&write.cell))
        .collect::<Result<Vec<_>, _>>()?;
    for (addr, write) in addrs.into_iter().zip(cells) {
        sheet.update(addr, |cell| {
            if let Some(input) = &write.input {
                cell.raw = Atom(input.clone());
            }
            if let Some(format) = write.format {
                cell.format = format;
            }
            if let Some(bold) = write.bold {
                cell.bold = bold;
            }
        });
    }
    Ok(())
}

/// Rewrite the sheet `id` for one inserted or deleted row or column `edit`:
/// the formulas pointing past it, and the charts and comments it cuts into.
/// The line itself is inserted or removed by the caller.
fn rewrite_for(book: &mut Workbook, id: &Id, edit: GridEdit) -> Result<Layout, Refusal> {
    let sheet = book.sheet_mut(id)?;
    let layout = sheet.layout();
    let cells: Vec<(Id, Id, String)> = sheet
        .cells
        .cells()
        .filter(|(_, _, cell)| cell.raw.0.starts_with('='))
        .map(|(row, column, cell)| (row, column, cell.raw.0.clone()))
        .collect();
    for (row, column, raw) in cells {
        let moved = crate::formula::remap_refs(&raw, edit);
        if moved != raw
            && let Some(cell) = sheet.cells.cell_mut(&row, &column)
        {
            cell.raw = Atom(moved);
        }
    }
    if !matches!(edit, GridEdit::DeleteRow(_) | GridEdit::DeleteColumn(_)) {
        return Ok(layout);
    }
    let charts: Vec<Id> = sheet.charts.ids().cloned().collect();
    for chart in charts {
        let moved = sheet
            .charts
            .get(&chart)
            .and_then(|it| reanchored(&layout, edit, &it.area));
        match moved {
            Some(area) => {
                if let Some(it) = sheet.charts.get_mut(&chart) {
                    it.area = area;
                }
            }
            None => {
                sheet.charts.remove(&chart);
            }
        }
    }
    let comments: Vec<Id> = book
        .comments
        .iter()
        .filter(|(_, comment)| comment.sheet == *id)
        .map(|(comment, _)| comment.clone())
        .collect();
    for comment in comments {
        let moved = book
            .comments
            .get(&comment)
            .and_then(|it| reanchored(&layout, edit, &it.area));
        match moved {
            Some(area) => {
                if let Some(it) = book.comments.get_mut(&comment) {
                    it.area = area;
                }
            }
            None => {
                book.comments.remove(&comment);
            }
        }
    }
    Ok(layout)
}

/// Where `area` is after deleting the line `edit` names, by the ids of the
/// lines then at its corners: a deleted first or last line gives way to its
/// neighbour; `None` when every cell of it goes.
fn reanchored(layout: &Layout, edit: GridEdit, area: &Area) -> Option<Area> {
    let (start, end) = Sheet::corners(layout, area)?;
    let (start, end) = edit.map_range(start, end)?;
    // Positions after the delete name the line that slides up into place.
    let line = |at: u32, axis: &[Id], gone: Option<u32>| -> Option<Id> {
        let at = match gone {
            Some(gone) if at >= gone => at + 1,
            _ => at,
        };
        axis.get(usize::try_from(at).ok()?).cloned()
    };
    let (gone_row, gone_column) = match edit {
        GridEdit::DeleteRow(at) => (Some(at), None),
        GridEdit::DeleteColumn(at) => (None, Some(at)),
        _ => (None, None),
    };
    Some(Area {
        top: line(start.1, layout.rows(), gone_row)?,
        left: line(start.0, layout.columns(), gone_column)?,
        bottom: line(end.1, layout.rows(), gone_row)?,
        right: line(end.0, layout.columns(), gone_column)?,
    })
}

#[resource(
    kind = "sheets.workbook",
    label = "Workbook",
    children = Workbook::annotations,
    search = Workbook::search,
    answer = crate::cell_actions::answer,
    params = verbs,
)]
impl Workbook {
    /// Add a sheet.
    #[action]
    pub fn new_sheet(
        &mut self,
        cx: &mut ActionCx,
        #[param("The new sheet's name; the next free SheetN when none")] name: Option<String>,
        #[param("The sheet it goes after; the end when none")] after: Option<Id>,
    ) -> Result<(), Refusal> {
        let name = name
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| self.next_sheet_name());
        let at = match &after {
            Some(after) => {
                self.sheets
                    .position(after)
                    .ok_or_else(|| Refusal::new("no such sheet"))?
                    + 1
            }
            None => self.sheets.len(),
        };
        self.sheets.insert(at, cx.id(), Sheet::named(name));
        Ok(())
    }

    /// Duplicate a sheet.
    #[action(item = sheets)]
    pub fn duplicate_sheet(&mut self, cx: &mut ActionCx, sheet: &Id) -> Result<(), Refusal> {
        let original = self
            .sheet(sheet)
            .ok_or_else(|| Refusal::new("no such sheet"))?;
        let mut copy = original.clone();
        copy.name = Atom(format!("{} copy", original.name.0));
        copy.source = None;
        let at = self
            .sheets
            .position(sheet)
            .map_or(self.sheets.len(), |at| at + 1);
        self.sheets.insert(at, cx.id(), copy);
        Ok(())
    }

    /// Delete a sheet, with its comments.
    #[action(destructive, writes = comments, item = sheets)]
    pub fn delete_sheet(&mut self, sheet: &Id) -> Result<(), Refusal> {
        self.sheets
            .remove(sheet)
            .ok_or_else(|| Refusal::new("no such sheet"))?;
        let gone: Vec<Id> = self
            .comments
            .iter()
            .filter(|(_, comment)| comment.sheet == *sheet)
            .map(|(id, _)| id.clone())
            .collect();
        for comment in gone {
            self.comments.remove(&comment);
        }
        Ok(())
    }

    /// Set cells from a start cell, row by row.
    #[action(answers, item = sheets)]
    pub fn set_cells(
        &mut self,
        sheet: &Id,
        #[param("Top-left cell, e.g. A1")] start: String,
        #[param(
            "Rows of values written rightward and down from start; \"=…\" is a formula, \"\" clears, null skips"
        )]
        rows: Vec<Vec<serde_json::Value>>,
    ) -> Result<(), Refusal> {
        let start = cell_of(&start)?;
        let mut cells = Vec::new();
        for (dr, row) in (0u32..).zip(&rows) {
            for (dc, value) in (0u32..).zip(row) {
                let input = match value {
                    serde_json::Value::Null => continue,
                    serde_json::Value::String(text) => text.clone(),
                    serde_json::Value::Number(number) => number.to_string(),
                    serde_json::Value::Bool(true) => "TRUE".into(),
                    serde_json::Value::Bool(false) => "FALSE".into(),
                    _ => {
                        return Err(Refusal::new(
                            "cell values are strings, numbers, booleans or null",
                        ));
                    }
                };
                let addr = (start.0.saturating_add(dc), start.1.saturating_add(dr));
                cells.push(CellWrite::at(addr, input, None));
            }
        }
        if cells.is_empty() {
            return Err(Refusal::new("rows hold no values to write"));
        }
        write_into(self.sheet_mut(sheet)?, &cells)
    }

    /// Write cells: values, formulas, formats and bold.
    #[action(answers, item = sheets)]
    pub fn write_cells(
        &mut self,
        sheet: &Id,
        #[param("The cells to write; what a cell leaves out stays as it is")] cells: Vec<CellWrite>,
    ) -> Result<(), Refusal> {
        if cells.is_empty() {
            return Err(Refusal::new("name at least one cell"));
        }
        write_into(self.sheet_mut(sheet)?, &cells)
    }

    /// Format cells.
    #[action(answers, item = sheets)]
    pub fn format_cells(
        &mut self,
        sheet: &Id,
        #[param("Cell or range, e.g. A1:D1")] range: String,
        #[param("Number format")] format: Option<CellFormat>,
        #[param("Bold text on or off")] bold: Option<bool>,
    ) -> Result<(), Refusal> {
        let (start, end) = range_of(&range)?;
        if format.is_none() && bold.is_none() {
            return Err(Refusal::new("give a format, bold or both"));
        }
        let cells: Vec<CellWrite> = (start.1..=end.1)
            .flat_map(|row| (start.0..=end.0).map(move |col| (col, row)))
            .map(|addr| CellWrite {
                cell: addr_to_a1(addr.0, addr.1),
                input: None,
                format,
                bold,
            })
            .collect();
        write_into(self.sheet_mut(sheet)?, &cells)
    }

    /// Sort rows by a column.
    #[action(answers, item = sheets)]
    pub fn sort_rows(
        &mut self,
        sheet: &Id,
        #[param("Rows to reorder, e.g. A2:C9 (leave the header out)")] range: String,
        #[param("Column letter to sort by, e.g. C")] by: String,
        #[param("Largest first; smallest first when none")] descending: Option<bool>,
    ) -> Result<(), Refusal> {
        let range = range_of(&range)?;
        let by = letters_to_col(by.trim())
            .filter(|by| (range.0.0..=range.1.0).contains(by))
            .ok_or_else(|| Refusal::new("sort by a column letter inside the range"))?;
        let target = self.sheet_mut(sheet)?;
        let shown = crate::cell_actions::shown_cells(&target.snapshot());
        let writes =
            crate::cell_actions::sort_writes(&shown, range, by, descending.unwrap_or(false));
        write_into(target, &writes)
    }

    /// Insert rows above a row.
    #[action(item = sheets)]
    pub fn insert_rows(
        &mut self,
        cx: &mut ActionCx,
        sheet: &Id,
        #[param("Row number as shown (1-based) the new rows go above")] at: u32,
        #[param("How many rows; one when none")] count: Option<u32>,
    ) -> Result<(), Refusal> {
        let at = at
            .checked_sub(1)
            .filter(|at| *at <= MAX_ROW)
            .ok_or_else(|| Refusal::new("rows are numbered from 1"))?;
        let count = count.unwrap_or(1).clamp(1, 1000);
        self.sheet_mut(sheet)?.reach((0, at));
        for _ in 0..count {
            rewrite_for(self, sheet, GridEdit::InsertRow(at))?;
            let id = cx.id();
            self.sheet_mut(sheet)?.cells.insert_row(at as usize, id);
        }
        Ok(())
    }

    /// Delete rows.
    #[action(destructive, answers, writes = comments, item = sheets)]
    pub fn delete_rows(
        &mut self,
        sheet: &Id,
        #[param("Row numbers as shown (1-based) to delete; rows below move up")] rows: Vec<u32>,
    ) -> Result<(), Refusal> {
        let mut rows = rows
            .into_iter()
            .map(|row| {
                row.checked_sub(1)
                    .ok_or_else(|| Refusal::new("rows are numbered from 1"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if rows.is_empty() {
            return Err(Refusal::new("name at least one row"));
        }
        rows.sort_unstable();
        rows.dedup();
        for at in rows.into_iter().rev() {
            if self
                .sheet_mut(sheet)?
                .layout()
                .rows()
                .get(at as usize)
                .is_none()
            {
                continue;
            }
            let layout = rewrite_for(self, sheet, GridEdit::DeleteRow(at))?;
            if let Some(id) = layout.rows().get(at as usize) {
                self.sheet_mut(sheet)?.cells.remove_row(id);
            }
        }
        Ok(())
    }

    /// Insert columns left of a column.
    #[action(item = sheets)]
    pub fn insert_columns(
        &mut self,
        cx: &mut ActionCx,
        sheet: &Id,
        #[param("Column letter the new columns go left of, e.g. C")] at: String,
        #[param("How many columns; one when none")] count: Option<u32>,
    ) -> Result<(), Refusal> {
        let at = letters_to_col(at.trim())
            .filter(|at| *at <= MAX_COLUMN)
            .ok_or_else(|| Refusal::new("name a column by its letter, e.g. C"))?;
        let count = count.unwrap_or(1).clamp(1, 1000);
        self.sheet_mut(sheet)?.reach((at, 0));
        for _ in 0..count {
            rewrite_for(self, sheet, GridEdit::InsertColumn(at))?;
            let id = cx.id();
            self.sheet_mut(sheet)?.cells.insert_column(at as usize, id);
        }
        Ok(())
    }

    /// Delete columns.
    #[action(destructive, writes = comments, item = sheets)]
    pub fn delete_columns(
        &mut self,
        sheet: &Id,
        #[param("Column letters to delete, e.g. [\"C\"]; columns right of them move left")]
        columns: Vec<String>,
    ) -> Result<(), Refusal> {
        let mut columns = columns
            .iter()
            .map(|letters| {
                letters_to_col(letters.trim())
                    .ok_or_else(|| Refusal::new(format!("`{letters}` is not a column letter")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if columns.is_empty() {
            return Err(Refusal::new("name at least one column"));
        }
        columns.sort_unstable();
        columns.dedup();
        for at in columns.into_iter().rev() {
            if self
                .sheet_mut(sheet)?
                .layout()
                .columns()
                .get(at as usize)
                .is_none()
            {
                continue;
            }
            let layout = rewrite_for(self, sheet, GridEdit::DeleteColumn(at))?;
            if let Some(id) = layout.columns().get(at as usize) {
                self.sheet_mut(sheet)?.cells.remove_column(id);
            }
        }
        Ok(())
    }

    /// Import sheets.
    ///
    /// Adds the sheets of a file as new sheets, in one step; into a new,
    /// still blank workbook they replace its empty first sheet.
    #[action]
    pub fn import_sheets(
        &mut self,
        cx: &mut ActionCx,
        #[param("The sheets, each with its name and cells")] sheets: Vec<ImportedSheet>,
        #[param("The file they come from, which opening again reveals")] source: Option<String>,
    ) -> Result<(), Refusal> {
        if sheets.is_empty() || sheets.len() > MAX_IMPORTED_SHEETS {
            return Err(Refusal::new("import between 1 and 64 sheets"));
        }
        if sheets.iter().map(|sheet| sheet.cells.len()).sum::<usize>() > MAX_IMPORTED_CELLS {
            return Err(Refusal::new("an import brings at most 65,536 cells"));
        }
        let blank = self
            .is_blank()
            .then(|| self.sheets.ids().next().cloned())
            .flatten();
        for imported in sheets {
            let mut sheet = Sheet::named(imported.name.trim());
            sheet.source = source.clone().map(Atom);
            write_into(&mut sheet, &imported.cells)?;
            self.sheets.push(cx.id(), sheet);
        }
        if let Some(blank) = blank {
            self.sheets.remove(&blank);
        }
        Ok(())
    }

    /// Chart a range.
    #[action(item = sheets)]
    pub fn add_chart(
        &mut self,
        cx: &mut ActionCx,
        sheet: &Id,
        #[param("The cells to chart, e.g. B2:D6")] range: String,
        #[param("Chart type")] kind: ChartKind,
        #[param("Title")] title: String,
    ) -> Result<(), Refusal> {
        let (start, end) = range_of(&range)?;
        let target = self.sheet_mut(sheet)?;
        let area = target
            .area(start, end)
            .ok_or_else(|| Refusal::new("the range is outside the sheet"))?;
        let title = title.trim();
        target.charts.push(
            cx.id(),
            Chart {
                title: Atom(if title.is_empty() { "Chart" } else { title }.to_owned()),
                kind,
                area,
            },
        );
        Ok(())
    }

    /// Edit a chart: its title, and its type and cells when given.
    #[action(item = sheets)]
    pub fn edit_chart(
        &mut self,
        sheet: &Id,
        #[param("The chart")] chart: Id,
        #[param("Title")] title: String,
        #[param("Chart type; none keeps it")] kind: Option<ChartKind>,
        #[param("The cells to chart, e.g. B2:D6; none keeps them")] range: Option<CellRange>,
    ) -> Result<(), Refusal> {
        let target = self.sheet_mut(sheet)?;
        let area = match range {
            Some(range) => Some(
                target
                    .area(range.start, range.end)
                    .ok_or_else(|| Refusal::new("the range is outside the sheet"))?,
            ),
            None => None,
        };
        let chart = target
            .charts
            .get_mut(&chart)
            .ok_or_else(|| Refusal::new("no such chart"))?;
        let title = title.trim();
        if title.len() > MAX_ANNOTATION_BYTES {
            return Err(Refusal::new("a title of at most 4 KiB"));
        }
        chart.title = Atom(if title.is_empty() { "Chart" } else { title }.to_owned());
        if let Some(kind) = kind {
            chart.kind = kind;
        }
        if let Some(area) = area {
            chart.area = area;
        }
        Ok(())
    }

    /// Delete a chart.
    #[action(destructive, item = sheets)]
    pub fn delete_chart(
        &mut self,
        sheet: &Id,
        #[param("The chart")] chart: Id,
    ) -> Result<(), Refusal> {
        self.sheet_mut(sheet)?
            .charts
            .remove(&chart)
            .map(drop)
            .ok_or_else(|| Refusal::new("no such chart"))
    }

    /// Comment on cells.
    #[action(writes = comments, item = sheets)]
    pub fn comment(
        &mut self,
        cx: &mut ActionCx,
        sheet: &Id,
        #[param("The cells it is about, e.g. B2:E2")] range: String,
        #[param("Comment")] text: String,
    ) -> Result<(), Refusal> {
        let (start, end) = range_of(&range)?;
        let body = annotation(&text)?;
        let author = Atom(cx.author().unwrap_or_default().to_owned());
        let id = cx.id();
        let area = self
            .sheet_mut(sheet)?
            .area(start, end)
            .ok_or_else(|| Refusal::new("the range is outside the sheet"))?;
        self.comments.push(
            id,
            Comment {
                sheet: sheet.clone(),
                area,
                author,
                body,
                resolved: false,
                replies: tilo_resource::Collection::new(),
            },
        );
        Ok(())
    }

    /// Reply to a comment.
    #[action(item = comments)]
    pub fn reply(
        &mut self,
        cx: &mut ActionCx,
        comment: &Id,
        #[param("Reply")] text: String,
    ) -> Result<(), Refusal> {
        let body = annotation(&text)?;
        let author = Atom(cx.author().unwrap_or_default().to_owned());
        let id = cx.id();
        self.comments
            .get_mut(comment)
            .ok_or_else(|| Refusal::new("the comment no longer exists"))?
            .replies
            .push(id, Reply { author, body });
        Ok(())
    }

    /// Edit a comment's text.
    #[action(item = comments)]
    pub fn edit_comment(
        &mut self,
        comment: &Id,
        #[param("Comment")] body: String,
    ) -> Result<(), Refusal> {
        let body = annotation(&body)?;
        self.comments
            .get_mut(comment)
            .ok_or_else(|| Refusal::new("the comment no longer exists"))?
            .body = body;
        Ok(())
    }

    /// Delete a comment.
    #[action(destructive, item = comments)]
    pub fn delete_comment(&mut self, comment: &Id) -> Result<(), Refusal> {
        self.comments
            .remove(comment)
            .map(drop)
            .ok_or_else(|| Refusal::new("the comment no longer exists"))
    }
}

impl Workbook {
    /// The charts and comment threads of every sheet, as places that open
    /// where they are and offer what can be done to them.
    fn annotations(&self) -> Vec<Derived<Self>> {
        let mut places = Vec::new();
        for (sheet_id, sheet) in self.sheets.iter() {
            let layout = sheet.layout();
            let shared = std::sync::Arc::new(layout.clone());
            for chart in sheet.chart_snapshots(&layout) {
                let id = Id::from_raw(chart.id.clone());
                let fields = Self::paths().sheets.item(sheet_id).charts.item(&id);
                let (kind, area, laid_out) =
                    (fields.kind, fields.area.path().clone(), shared.clone());
                places.push(
                    Derived::new(chart.title.clone())
                        .of_kind("chart")
                        .identified(format!("chart:{}", chart.id))
                        .with_effect(&SheetsEffect::Open {
                            sheet: sheet_id.as_str().to_owned(),
                            col: chart.start.0,
                            row: chart.start.1,
                        })
                        .with(
                            Self::EDIT_CHART
                                .bind(verbs::EditChart::paths().sheet, sheet_id.clone())
                                .and(verbs::EditChart::paths().chart, id.clone()),
                        )
                        .with_edit_asking(
                            "chart-type",
                            "Chart type",
                            "kind",
                            "column or line",
                            move |value: ChartKind| kind.set(value),
                        )
                        .with_edit_asking(
                            "chart-cells",
                            "Chart cells",
                            "range",
                            "The cells to chart, e.g. B2:D6",
                            move |range: CellRange| {
                                area.set(crate::workbook::Area::of(
                                    &laid_out,
                                    range.start,
                                    range.end,
                                ))
                            },
                        )
                        .with(
                            Self::DELETE_CHART
                                .bind(verbs::DeleteChart::paths().sheet, sheet_id.clone())
                                .and(verbs::DeleteChart::paths().chart, id),
                        ),
                );
            }
            for thread in Sheet::comment_snapshots(sheet_id, &self.comments, &layout) {
                let id = Id::from_raw(thread.id.clone());
                places.push(
                    Derived::new(format!(
                        "{} {}",
                        range_label(thread.start, thread.end),
                        thread.body
                    ))
                    .of_kind("comment")
                    .identified(format!("comment:{}", thread.id))
                    .with_effect(&SheetsEffect::Open {
                        sheet: sheet_id.as_str().to_owned(),
                        col: thread.start.0,
                        row: thread.start.1,
                    })
                    .with(Self::REPLY.bind(verbs::Reply::paths().comment, id.clone()))
                    .with(Self::EDIT_COMMENT.bind(verbs::EditComment::paths().comment, id.clone()))
                    .with(Self::DELETE_COMMENT.bind(verbs::DeleteComment::paths().comment, id)),
                );
            }
        }
        places
    }

    /// Every sheet by name and every populated cell by reference, input
    /// and what it shows; a hit opens it.
    fn search(&self) -> Vec<SearchEntry> {
        let mut entries = Vec::new();
        for (id, sheet) in self.sheets.iter() {
            entries.push(
                SearchEntry::new(sheet.name.0.clone(), sheet.name.0.clone())
                    .anchored(tilo_resource::items::item_resource_id::<Self>("sheets", id)),
            );
            for cell in crate::cell_actions::shown_cells(&sheet.snapshot()) {
                let reference = addr_to_a1(cell.col, cell.row);
                let title = format!("{}!{reference}", sheet.name.0);
                let text = format!("{reference} {title} {} {}", cell.raw, cell.display);
                entries.push(
                    SearchEntry::new(title, text).anchored(cell_anchor(id, (cell.col, cell.row))),
                );
            }
        }
        entries
    }
}

/// The search anchor of the cell at `addr` of the sheet `sheet`.
pub fn cell_anchor(sheet: &Id, (col, row): Addr) -> String {
    format!("cell:{}:{col}:{row}", sheet.as_str())
}

/// The sheet and cell a search anchor names.
pub fn parse_cell_anchor(anchor: &str) -> Option<(Id, Addr)> {
    let rest = anchor.strip_prefix("cell:")?;
    let (rest, row) = rest.rsplit_once(':')?;
    let (sheet, col) = rest.rsplit_once(':')?;
    Some((Id::from_raw(sheet), (col.parse().ok()?, row.parse().ok()?)))
}

/// Framework app: the sheets grid is the surface over [`Workbook`].
pub struct SheetsShell;

/// Every screenshot scenario: the sample workbook with one surface of the app
/// in view.
const SCENARIOS: &[&str] = &[
    "file",
    "format",
    "more",
    "docsettings",
    "editing",
    "pointmode",
    "summary",
    "multiselect",
    "tabmenu",
    "tabcolour",
    "tabrename",
    "fbarselect",
    "commandpalette",
    "fillseries",
    "scrollbars",
    "pasteformula",
    "error",
    "conflict",
    "reducedmotion",
    "charts",
    "comments",
    "bold",
];

/// When the sample workbook of the annotated scenarios was written.
const SAMPLE_WRITTEN_MS: i64 = 1_767_225_600_000;

/// Whether `scenario` shows the sample's charts and comments.
fn annotated(scenario: Option<&str>) -> bool {
    matches!(scenario, Some("charts" | "comments"))
}

impl ResourceApp for SheetsShell {
    type Resource = Workbook;
    const ID: &'static str = "so.tilo.sheets";
    const TITLE: &'static str = "Sheets";
    const ICON: Option<&'static str> = Some("sheets");

    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&CATALOG)
    }

    /// The annotated scenarios show a workbook other people wrote: Alex
    /// shared the budget and commented on it, Sam replied.
    fn fixture(fixture: &mut Fixture<Workbook>) {
        if !annotated(fixture.scenario()) {
            return;
        }
        let versions = crate::sample::workbook()
            .into_iter()
            .map(|invoke| ("Alex", invoke))
            .chain(crate::sample::comments());
        for (at, (author, invoke)) in (0_i64..).zip(versions) {
            fixture.version(author, SAMPLE_WRITTEN_MS + at * 60_000, invoke);
        }
    }

    fn scenario(name: Option<&str>) -> Vec<Invoke<Workbook>> {
        if annotated(name) {
            let mut invokes = crate::sample::charts();
            invokes.extend(crate::sample::resolve_comment());
            return invokes;
        }
        let mut invokes = crate::sample::workbook();
        if name == Some("fillseries") {
            invokes.extend(crate::sample::fill_seeds());
        }
        invokes
    }

    fn view(cx: &mut ViewCx<Workbook>) -> Element {
        let app = SheetsApp::new();
        // A search hit, or an agent's `open`, shows the sheet or cell it names.
        cx.on_open(app.opener());
        cx.surface(app)
    }
}

impl ResourceSurface<Workbook, SheetsEffect> for SheetsApp {
    fn effect(&mut self, effect: SheetsEffect, cx: &mut EffectCx<Workbook, Self>) -> EffectAnswer {
        self.answer_effect(effect, cx)
    }

    fn present(&mut self, state: &Rc<Workbook>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Workbook>) {
        self.attach_framework(cx.dispatcher(), cx.live());
        self.follow_locale(cx.settings::<tilo_locale::Locale>());
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }

    fn bound_declarations(&self) -> Vec<ActionDecl<Workbook>> {
        self.selection_declarations()
    }
}

#[cfg(test)]
mod tests;
