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

static CATALOG: EmbeddedCatalog =
    EmbeddedCatalog::new("en-US", LOCALES, "sheets-new-sheet").with_system_locale();

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

/// Rewrite `sheet` for one inserted or deleted row or column `edit`: the
/// formulas pointing past it, and the charts and comments it cuts into.
/// The line itself is inserted or removed by the caller.
fn rewrite_for(sheet: &mut Sheet, layout: &Layout, edit: GridEdit) {
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
    let deleting = matches!(edit, GridEdit::DeleteRow(_) | GridEdit::DeleteColumn(_));
    if !deleting {
        return;
    }
    // A deleted first or last line of an anchor gives way to its neighbour;
    // an anchor whose every cell goes is dropped.
    let after = |start: Addr, end: Addr| edit.map_range(start, end);
    let reanchor = |area: &Area| -> Option<Area> {
        let (start, end) = Sheet::corners(layout, area)?;
        let (start, end) = after(start, end)?;
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
    };
    let charts: Vec<Id> = sheet.charts.ids().cloned().collect();
    for id in charts {
        let moved = sheet
            .charts
            .get(&id)
            .and_then(|chart| reanchor(&chart.area));
        match moved {
            Some(area) => {
                if let Some(chart) = sheet.charts.get_mut(&id) {
                    chart.area = area;
                }
            }
            None => {
                sheet.charts.remove(&id);
            }
        }
    }
    let comments: Vec<Id> = sheet.comments.ids().cloned().collect();
    for id in comments {
        let moved = sheet
            .comments
            .get(&id)
            .and_then(|comment| reanchor(&comment.area));
        match moved {
            Some(area) => {
                if let Some(comment) = sheet.comments.get_mut(&id) {
                    comment.area = area;
                }
            }
            None => {
                sheet.comments.remove(&id);
            }
        }
    }
}

#[resource(
    kind = "sheets.workbook",
    label = "Workbook",
    children = Workbook::annotations,
    search = Workbook::search,
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
    #[action]
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

    /// Delete a sheet.
    #[action(destructive)]
    pub fn delete_sheet(&mut self, sheet: &Id) -> Result<(), Refusal> {
        self.sheets
            .remove(sheet)
            .map(drop)
            .ok_or_else(|| Refusal::new("no such sheet"))
    }

    /// Write cells: values, formulas, formats and bold.
    #[action]
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
    #[action]
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
    #[action]
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
    #[action]
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
        let target = self.sheet_mut(sheet)?;
        target.reach((0, at));
        for _ in 0..count {
            let layout = target.layout();
            rewrite_for(target, &layout, GridEdit::InsertRow(at));
            target.cells.insert_row(at as usize, cx.id());
        }
        Ok(())
    }

    /// Delete rows.
    #[action(destructive)]
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
        let target = self.sheet_mut(sheet)?;
        for at in rows.into_iter().rev() {
            let layout = target.layout();
            let Some(id) = layout.rows().get(at as usize).cloned() else {
                continue;
            };
            rewrite_for(target, &layout, GridEdit::DeleteRow(at));
            target.cells.remove_row(&id);
        }
        Ok(())
    }

    /// Insert columns left of a column.
    #[action]
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
        let target = self.sheet_mut(sheet)?;
        target.reach((at, 0));
        for _ in 0..count {
            let layout = target.layout();
            rewrite_for(target, &layout, GridEdit::InsertColumn(at));
            target.cells.insert_column(at as usize, cx.id());
        }
        Ok(())
    }

    /// Delete columns.
    #[action(destructive)]
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
        let target = self.sheet_mut(sheet)?;
        for at in columns.into_iter().rev() {
            let layout = target.layout();
            let Some(id) = layout.columns().get(at as usize).cloned() else {
                continue;
            };
            rewrite_for(target, &layout, GridEdit::DeleteColumn(at));
            target.cells.remove_column(&id);
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
    #[action]
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

    /// Chart another range.
    #[action]
    pub fn move_chart(
        &mut self,
        sheet: &Id,
        #[param("The chart")] chart: Id,
        #[param("The cells to chart, e.g. B2:D6")] range: String,
    ) -> Result<(), Refusal> {
        let (start, end) = range_of(&range)?;
        let target = self.sheet_mut(sheet)?;
        let area = target
            .area(start, end)
            .ok_or_else(|| Refusal::new("the range is outside the sheet"))?;
        target
            .charts
            .get_mut(&chart)
            .ok_or_else(|| Refusal::new("no such chart"))?
            .area = area;
        Ok(())
    }

    /// Delete a chart.
    #[action(destructive)]
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
    #[action]
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
        let target = self.sheet_mut(sheet)?;
        let area = target
            .area(start, end)
            .ok_or_else(|| Refusal::new("the range is outside the sheet"))?;
        target.comments.push(
            id,
            Comment {
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
    #[action]
    pub fn reply(
        &mut self,
        cx: &mut ActionCx,
        sheet: &Id,
        #[param("The comment")] comment: Id,
        #[param("Reply")] text: String,
    ) -> Result<(), Refusal> {
        let body = annotation(&text)?;
        let author = Atom(cx.author().unwrap_or_default().to_owned());
        let id = cx.id();
        self.sheet_mut(sheet)?
            .comments
            .get_mut(&comment)
            .ok_or_else(|| Refusal::new("the comment no longer exists"))?
            .replies
            .push(id, Reply { author, body });
        Ok(())
    }

    /// Delete a comment.
    #[action(destructive)]
    pub fn delete_comment(
        &mut self,
        sheet: &Id,
        #[param("The comment")] comment: Id,
    ) -> Result<(), Refusal> {
        self.sheet_mut(sheet)?
            .comments
            .remove(&comment)
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
            for chart in sheet.chart_snapshots(&layout) {
                let id = Id::from_raw(chart.id.clone());
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
                            Self::MOVE_CHART
                                .bind(verbs::MoveChart::paths().sheet, sheet_id.clone())
                                .and(verbs::MoveChart::paths().chart, id.clone()),
                        )
                        .with(
                            Self::DELETE_CHART
                                .bind(verbs::DeleteChart::paths().sheet, sheet_id.clone())
                                .and(verbs::DeleteChart::paths().chart, id),
                        ),
                );
            }
            for thread in sheet.comment_snapshots(&layout) {
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
                    .with(
                        Self::REPLY
                            .bind(verbs::Reply::paths().sheet, sheet_id.clone())
                            .and(verbs::Reply::paths().comment, id.clone()),
                    )
                    .with(
                        Self::DELETE_COMMENT
                            .bind(verbs::DeleteComment::paths().sheet, sheet_id.clone())
                            .and(verbs::DeleteComment::paths().comment, id),
                    ),
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

    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1280, 800)
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
        self.follow_locale(cx.settings::<crate::settings::Locale>());
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
