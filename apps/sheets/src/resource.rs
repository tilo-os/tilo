//! The workbook's verbs, the places derived from it and the app. Plain fields change through
//! the built-in field edits; the methods below address cells by position, as people and agents
//! do, and keep formulas, charts and comments pointing at the cells they meant.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
#[cfg(feature = "samples")]
use ui::framework::Fixture;
use ui::framework::{ResourceApp, ViewCx};
use ui::i18n::EmbeddedCatalog;
#[cfg(feature = "samples")]
use ui::tilo_resource::Invoke;
use ui::tilo_resource::{
    self, ActionCx, Atom, Derived, Id, JsonSchema, NonBlank, Refusal, SearchEntry, resource,
};
use ui::widget::Element;

use crate::actions::SheetsEffect;
use crate::app::SheetsApp;
use crate::bound::{Input, Lines};
use crate::model::{Addr, CellFormat, GridEdit, a1_to_addr, addr_to_a1, letters_to_col};
use crate::workbook::{
    Area, Chart, ChartKind, Comment, CommentCells, Layout, MAX_ANNOTATION_BYTES, MAX_COLUMN,
    MAX_ROW, Sheet, Workbook,
};
use tilo_rich_text::annotation::Reply;

ui::embedded_catalog!(pub(crate) static CATALOG, ["en-US", "de-DE"]);

/// The most cells one write, sort or import changes.
pub const MAX_WRITES: usize = 16 * 1024;
/// The most cells one import brings, over all its sheets.
pub const MAX_IMPORTED_CELLS: usize = 64 * 1024;
/// The most sheets one import brings.
pub const MAX_IMPORTED_SHEETS: usize = 64;

/// One cell a write changes, by its reference; what it leaves out stays as it is, and an input
/// of `""` clears it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CellWrite {
    pub cell: String,
    /// What to type: a value or a formula; an ISO date becomes a real date, and a leading apostrophe
    /// marks text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<CellFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bold: Option<bool>,
}

impl CellWrite {
    pub fn at(addr: Addr, input: impl Into<String>, format: Option<CellFormat>) -> Self {
        Self {
            cell: addr_to_a1(addr.0, addr.1),
            input: Some(input.into()),
            format,
            bold: None,
        }
    }
}

/// One sheet an import brings.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ImportedSheet {
    pub name: String,
    pub cells: Vec<CellWrite>,
    /// Whether the sheet comes hidden.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
}

/// A new sheet: where it goes, its name and, optionally, its first content.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SheetSpec {
    /// The new sheet's name; the next free `SheetN` when none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The sheet it goes after, by id or name; the end when none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<Id>,
    /// Its first content: rows of values from A1, where "=…" is a formula and an ISO date a date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<Vec<Vec<serde_json::Value>>>,
    /// Header names written as row 1, which records are keyed by.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub columns: Option<Vec<String>>,
    /// Rows from row 2 keyed by column; a key left out leaves its cell empty. In a formula
    /// `[Header]` is that column's cell in the row and `{row}` its number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub records: Option<Vec<serde_json::Value>>,
    /// Number formats of its ranges: general, number, currency, percent, date, time or text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub formats: Option<std::collections::BTreeMap<String, String>>,
    /// The spreadsheet's name, when the workbook has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workbook: Option<String>,
}

/// The format `name` names: a category or a style like `currency:EUR`, with bare money in the
/// workbook's currency.
fn format_named(name: &str, currency: tilo_locale::Currency) -> Result<CellFormat, Refusal> {
    let code = name.trim().to_lowercase();
    let code = match code.as_str() {
        "money" | "currency" => return Ok(CellFormat::Currency(currency)),
        "percentage" => "percent",
        other => other,
    };
    CellFormat::parse_code(code).ok_or_else(|| {
        Refusal::new(format!(
            "`{name}` is no format: general, number, currency, percent, date, time or text"
        ))
    })
}

/// `writes` with the formats their ranges ask for; a format on a cell no write names writes the
/// format alone.
fn with_formats(
    mut writes: Vec<CellWrite>,
    formats: &std::collections::BTreeMap<String, String>,
    currency: tilo_locale::Currency,
) -> Result<Vec<CellWrite>, Refusal> {
    for (range, name) in formats {
        let (start, end) = range_of(range)?;
        let format = format_named(name, currency)?;
        for row in start.1..=end.1 {
            for col in start.0..=end.0 {
                let cell = addr_to_a1(col, row);
                match writes.iter_mut().find(|write| write.cell == cell) {
                    Some(write) => write.format = Some(format),
                    None => writes.push(CellWrite {
                        cell,
                        format: Some(format),
                        ..CellWrite::default()
                    }),
                }
                if writes.len() > MAX_WRITES {
                    return Err(Refusal::new(format!("at most {MAX_WRITES} cells at once")));
                }
            }
        }
    }
    Ok(writes)
}

/// The writes of `rows` rightward and down from `start`.
fn row_writes(start: Addr, rows: &[Vec<serde_json::Value>]) -> Result<Vec<CellWrite>, Refusal> {
    let mut cells = Vec::new();
    for (dr, row) in (0u32..).zip(rows) {
        for (dc, value) in (0u32..).zip(row) {
            let addr = (start.0.saturating_add(dc), start.1.saturating_add(dr));
            if let Some((input, format)) = crate::cell_actions::cell_input(value)? {
                cells.push(CellWrite::at(addr, input, format));
            }
        }
    }
    Ok(cells)
}

/// A new sheet's first content, or `None` when it has none.
fn first_writes(new: &SheetSpec) -> Result<Option<Vec<CellWrite>>, Refusal> {
    match (&new.rows, &new.columns, &new.records) {
        (Some(_), Some(_), _) => Err(Refusal::new(
            "give `rows`, or `columns` with `records`, not both",
        )),
        (Some(_), None, Some(_)) => Err(Refusal::new(
            "`records` are keyed by `columns`: give those instead of `rows`",
        )),
        (Some(rows), None, None) => row_writes((0, 0), rows).map(Some),
        (None, Some(columns), records) => {
            crate::records::new_sheet_writes(columns, records.as_deref().unwrap_or_default())
                .map(Some)
        }
        (None, None, Some(records)) => {
            crate::records::new_sheet_writes(&crate::records::keys_in_order(records), records)
                .map(Some)
        }
        (None, None, None) => Ok(None),
    }
}

/// `write` with its input read as `set-cells` reads it; a named format wins.
fn typed_write(write: CellWrite) -> Result<CellWrite, Refusal> {
    let Some(input) = write.input.as_deref() else {
        return Ok(write);
    };
    if write.format == Some(CellFormat::Text) {
        return Ok(write);
    }
    let Some((input, format)) =
        crate::cell_actions::cell_input(&serde_json::Value::String(input.to_owned()))?
    else {
        return Ok(write);
    };
    Ok(CellWrite {
        input: Some(input),
        format: write.format.or(format),
        ..write
    })
}

fn cell_of(reference: &str) -> Result<Addr, Refusal> {
    a1_to_addr(reference.trim())
        .filter(|(col, row)| *col <= MAX_COLUMN && *row <= MAX_ROW)
        .ok_or_else(|| Refusal::new(format!("`{reference}` is not a cell such as B3")))
}

/// `A1` or `A1:D6`, normalised so the first corner is the top left.
pub fn parse_range(text: &str) -> Option<(Addr, Addr)> {
    let (a, b) = text.split_once(':').unwrap_or((text, text));
    let (a, b) = (a1_to_addr(a.trim())?, a1_to_addr(b.trim())?);
    let range = ((a.0.min(b.0), a.1.min(b.1)), (a.0.max(b.0), a.1.max(b.1)));
    (range.1.0 <= MAX_COLUMN && range.1.1 <= MAX_ROW).then_some(range)
}

/// A range as people write it: `B3`, or `B3:D9`.
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

/// `start..=end` as people write it.
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

/// Write `cells` into `sheet`, each changing only what it names, and answer the cells given an
/// input. An input is written as typed: [`Workbook::write`] binds the formulas among them.
fn write_into(sheet: &mut Sheet, cells: &[CellWrite]) -> Result<Vec<Addr>, Refusal> {
    if cells.len() > MAX_WRITES {
        return Err(Refusal::new(format!("at most {MAX_WRITES} cells at once")));
    }
    let addrs = cells
        .iter()
        .map(|write| cell_of(&write.cell))
        .collect::<Result<Vec<_>, _>>()?;
    let typed = addrs
        .iter()
        .zip(cells)
        .filter(|(_, write)| write.input.is_some())
        .map(|(addr, _)| *addr)
        .collect();
    sheet.update_all(addrs.into_iter().zip(cells).map(|(addr, write)| {
        (addr, move |cell: &mut crate::workbook::Cell| {
            if let Some(input) = &write.input {
                cell.raw = Input::typed(input.clone());
            }
            if let Some(format) = write.format {
                cell.format = format;
            }
            if let Some(bold) = write.bold {
                cell.bold = bold;
            }
        })
    }));
    Ok(typed)
}

impl Workbook {
    /// Write `cells` into sheet `id`, binding each formula among them to the rows, columns and
    /// sheets it names now. A formula written again as it reads keeps what it was bound to.
    fn write(&mut self, id: &Id, cells: &[CellWrite]) -> Result<(), Refusal> {
        let held: HashMap<Addr, (String, Input)> = {
            let lines = Lines::of(self);
            match (self.sheet(id), lines.layout(id)) {
                (Some(sheet), Some(layout)) => cells
                    .iter()
                    .filter(|write| {
                        write
                            .input
                            .as_deref()
                            .is_some_and(|input| input.starts_with('='))
                    })
                    .filter_map(|write| {
                        let addr = cell_of(&write.cell).ok()?;
                        let raw = &sheet.cell_at(&layout, addr)?.raw;
                        raw.is_formula()
                            .then(|| (addr, (raw.shown(&layout, &lines).into_owned(), raw.clone())))
                    })
                    .collect(),
                _ => HashMap::new(),
            }
        };
        let typed = write_into(self.sheet_mut(id)?, cells)?;
        self.bind(id, &typed, &held);
        Ok(())
    }

    /// Bind the formulas typed into `cells` of sheet `id` to what they name now. `held` is
    /// what a cell read before it was written, and its input then: a formula typed as that
    /// read is that input.
    fn bind(&mut self, id: &Id, cells: &[Addr], held: &HashMap<Addr, (String, Input)>) {
        let bound: Vec<(Id, Id, Input)> = {
            let lines = Lines::of(self);
            let (Some(sheet), Some(layout)) = (self.sheet(id), lines.layout(id)) else {
                return;
            };
            cells
                .iter()
                .filter_map(|addr| {
                    let (row, column) = layout.ids(*addr)?;
                    let raw = &sheet.cells.cell(row, column)?.raw;
                    if raw.is_bound() || !raw.is_formula() {
                        return None;
                    }
                    let input = match held.get(addr) {
                        Some((shown, input)) if shown == raw.text() => input.clone(),
                        _ => Input::bound(raw.text(), &layout, &lines),
                    };
                    Some((row.clone(), column.clone(), input))
                })
                .collect()
        };
        let Ok(sheet) = self.sheet_mut(id) else {
            return;
        };
        for (row, column, input) in bound {
            if let Some(cell) = sheet.cells.cell_mut(&row, &column) {
                cell.raw = input;
            }
        }
    }

    /// Refuse a write made against rows and columns that have moved since its caller read
    /// them: `seen` is the stamp that read answered ([`Workbook::seen`]). A caller that names
    /// none is not held to any.
    fn as_seen(&self, seen: Option<&str>) -> Result<(), Refusal> {
        match seen.map(str::trim) {
            Some(seen) if !seen.is_empty() && seen != self.seen() => Err(Refusal::new(
                "rows, columns or sheets moved since the read you name with `seen`, so these cell references may mean other cells now: read again and write against what that answers; nothing was written",
            )),
            _ => Ok(()),
        }
    }

    /// Bind every formula of the sheets `ids` that has no binding: sheets just made, whose
    /// formulas may read one another.
    fn bind_sheets(&mut self, ids: &[Id]) {
        for id in ids {
            let Some(sheet) = self.sheet(id) else {
                continue;
            };
            let layout = sheet.layout();
            let cells: Vec<Addr> = sheet
                .cells
                .cells()
                .filter(|(_, _, cell)| cell.raw.is_formula() && !cell.raw.is_bound())
                .filter_map(|(row, column, _)| layout.addr(&row, &column))
                .collect();
            self.bind(id, &cells, &HashMap::new());
        }
    }

    /// Bind every formula with no binding that names sheet `id`, before the sheet is renamed
    /// or deleted: bound to the sheet itself, it reads it under any name, and `#REF!` once it
    /// is gone, with nothing rewritten.
    fn bind_naming(&mut self, id: &Id) {
        let Some(named) = self.name_reaching(id) else {
            return;
        };
        let ids: Vec<Id> = self.sheets.ids().cloned().collect();
        for on in &ids {
            let Some(sheet) = self.sheet(on) else {
                continue;
            };
            let layout = sheet.layout();
            let cells: Vec<Addr> = sheet
                .cells
                .cells()
                .filter(|(_, _, cell)| {
                    let raw = &cell.raw;
                    if raw.is_bound() || !raw.is_formula() || !raw.text().contains('!') {
                        return false;
                    }
                    let mut reads = false;
                    crate::refs::each_reference(raw.text(), |sheet, _, _| {
                        reads |=
                            sheet.is_some_and(|written| crate::refs::fold_sheet(written) == named);
                    });
                    reads
                })
                .filter_map(|(row, column, _)| layout.addr(&row, &column))
                .collect();
            self.bind(on, &cells, &HashMap::new());
        }
    }
}

/// The formulas a structural change of one sheet may move, found once for the whole action:
/// every formula of that sheet, and on the other sheets those that name a sheet.
struct Reshape {
    sheet: Id,
    /// The name formulas reach the sheet by.
    named: Option<String>,
    formulas: Vec<(Id, Vec<(Id, Id)>)>,
}

impl Reshape {
    fn of(book: &Workbook, sheet: &Id) -> Self {
        let named = book.name_reaching(sheet);
        let formulas = book
            .sheets
            .iter()
            .filter(|(id, _)| *id == sheet || named.is_some())
            .map(|(id, other)| {
                let own = id == sheet;
                let cells: Vec<(Id, Id)> = other
                    .cells
                    .cells()
                    .filter(|(_, _, cell)| {
                        cell.raw.is_formula() && (own || cell.raw.text().contains('!'))
                    })
                    .map(|(row, column, _)| (row, column))
                    .collect();
                (id.clone(), cells)
            })
            .filter(|(_, cells)| !cells.is_empty())
            .collect();
        Self {
            sheet: sheet.clone(),
            named,
            formulas,
        }
    }

    /// Ready the workbook for one inserted or deleted row or column of the sheet. No formula is
    /// rewritten: each reads its lines by id, so it follows them by itself. A formula with no
    /// binding that the change would move gains one first, to what it reads now; a range that
    /// begins or ends on a deleted line shrinks to the line beside it; and the charts and
    /// comments a delete cuts into move or go. Answers the sheet's layout before the change.
    fn apply(&self, book: &mut Workbook, edit: GridEdit) -> Result<Layout, Refusal> {
        let doomed = match edit {
            GridEdit::DeleteRow(at) => Some((true, at)),
            GridEdit::DeleteColumn(at) => Some((false, at)),
            GridEdit::InsertRow(_) | GridEdit::InsertColumn(_) => None,
        };
        let (bound, layout) = {
            let lines = Lines::of(book);
            let layout = lines
                .layout(&self.sheet)
                .ok_or_else(|| Refusal::new("no such sheet"))?;
            let mut bound: Vec<(&Id, &Id, &Id, Input)> = Vec::new();
            for (id, cells) in &self.formulas {
                let own = *id == self.sheet;
                let (Some(held), Some(there)) = (book.sheet(id), lines.layout(id)) else {
                    continue;
                };
                for (row, column) in cells {
                    let Some(raw) = held.cells.cell(row, column).map(|cell| &cell.raw) else {
                        continue;
                    };
                    let mut next = None;
                    if !raw.is_bound() {
                        let text = raw.text();
                        let moved =
                            crate::refs::remap_sheet_refs(text, edit, own, self.named.as_deref());
                        if moved != text {
                            next = Some(Input::bound(text, &there, &lines));
                        }
                    }
                    if let Some((rows, at)) = doomed {
                        let shrunk = next.as_ref().unwrap_or(raw).without_line(
                            (id, &there),
                            &lines,
                            &self.sheet,
                            rows,
                            at,
                        );
                        if shrunk.is_some() {
                            next = shrunk;
                        }
                    }
                    if let Some(input) = next {
                        bound.push((id, row, column, input));
                    }
                }
            }
            (bound, Layout::clone(&layout))
        };
        for (id, row, column, input) in bound {
            if let Some(cell) = book.sheet_mut(id)?.cells.cell_mut(row, column) {
                cell.raw = input;
            }
        }
        let id = &self.sheet;
        if doomed.is_none() {
            return Ok(layout);
        }
        let sheet = book.sheet_mut(id)?;
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
            .filter(|(_, comment)| comment.anchor.sheet == *id)
            .map(|(comment, _)| comment.clone())
            .collect();
        for comment in comments {
            let moved = book
                .comments
                .get(&comment)
                .and_then(|it| reanchored(&layout, edit, &it.anchor.area));
            match moved {
                Some(area) => {
                    if let Some(it) = book.comments.get_mut(&comment) {
                        it.anchor.area = area;
                    }
                }
                None => {
                    book.comments.remove(&comment);
                }
            }
        }
        Ok(layout)
    }
}

/// Where `area` is after deleting a line, by the ids of the lines at its corners; `None` when
/// every cell of it goes.
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

/// Bring a workbook of an older version up to this one. Version 1 stored each mark's value in
/// a comment's paragraphs flat beside its variant's name; it is stored by its variant now.
/// Version 3 stores what a formula's references are bound to after its text
/// ([`crate::bound::Input`]): a cell of version 2 holds the text alone and reads by position
/// as it did, so none is rewritten, and each gains its binding when it is next written. The
/// version still changes, because a build that knows no binding would take the text for the
/// whole formula.
fn upgrade(
    doc: &mut ui::tilo_resource::Doc,
    from: u32,
) -> Result<(), ui::tilo_resource::ModelError> {
    match from {
        1 => tilo_rich_text::annotation::store_bodies_by_variant(
            doc,
            &ui::automerge::ROOT,
            "comments",
        ),
        2 => Ok(()),
        _ => Err(ui::tilo_resource::ModelError::invalid(format!(
            "no workbook migration from {from}"
        ))),
    }
}

#[resource(
    kind = "sheets.workbook",
    children = Workbook::annotations,
    search = Workbook::search,
    read_content = Workbook::read_cells,
    document_child = Workbook::document_child,
    answer = crate::cell_actions::answer,
    params = verbs,
    version = 3,
    migrate = upgrade,
)]
impl Workbook {
    /// Add a sheet.
    ///
    /// With content (rows, or columns and records, and their formats) it is one step that answers
    /// the cells it wrote; a new workbook's blank Sheet1 gives way to it.
    #[action(answers, label = "sheets-add-sheet")]
    pub fn new_sheet(
        &mut self,
        cx: &mut ActionCx,
        #[param(flatten)] new: SheetSpec,
    ) -> Result<(), Refusal> {
        let mut writes = first_writes(&new)?;
        if let Some(formats) = &new.formats {
            writes = Some(with_formats(
                writes.unwrap_or_default(),
                formats,
                self.currency(),
            )?);
        }
        let blank = writes
            .as_ref()
            .filter(|_| self.is_blank())
            .and_then(|_| self.sheets.ids().next().cloned());
        let name = new
            .name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map_or_else(
                || {
                    if blank.is_some() {
                        crate::workbook::FIRST_SHEET_NAME.to_owned()
                    } else {
                        self.next_sheet_name()
                    }
                },
                str::to_owned,
            );
        let at = match &new.after {
            Some(after) => {
                let after = self.sheet_key(after)?;
                self.sheets.position(&after).unwrap_or(self.sheets.len()) + 1
            }
            None => self.sheets.len(),
        };
        let mut sheet = Sheet::named(name);
        if let Some(writes) = &writes {
            write_into(&mut sheet, writes)?;
        }
        if let Some(title) = new.workbook.as_deref().map(str::trim)
            && !title.is_empty()
            && self.title.is_none()
        {
            self.title = Some(title.to_owned());
        }
        let made = self.sheets.insert(at, cx.id(), sheet);
        if let Some(blank) = blank {
            self.sheets.remove(&blank);
        }
        self.bind_sheets(&[made]);
        Ok(())
    }

    /// Name the workbook, so it is found and listed by that name.
    #[action]
    pub fn set_title(
        &mut self,
        #[param("The spreadsheet's name, as people call it")] title: NonBlank,
    ) {
        self.title = Some(title.into_string());
    }

    /// Set records: rows written by the headers in row 1.
    #[action(answers, item = sheets, audience = "agents")]
    pub fn set_records(
        &mut self,
        sheet: &Id,
        #[param(
            "Rows as objects keyed by the headers in row 1, e.g. [{\"Item\": \"Venue\", \"Total\": \"=[Price]*[Qty]\"}]; a key left out or null keeps its cell, \"\" clears. In a formula [Header] is that column's cell in the same row and {row} the row's number"
        )]
        records: Vec<serde_json::Value>,
        #[param(
            "Data row the first record goes to, 1 = the row under the headers (sheet row 2), the rest below it; leave it out to append after the last used row. A record whose first-column value differs from the one its filled row holds is refused: the rows moved since they were read"
        )]
        row: Option<u32>,
        #[param(
            "An empty sheet's headers, in order; left out, they are the records' keys as they first come"
        )]
        columns: Option<Vec<String>>,
        #[param(
            "The header whose value identifies each record's row: a record updates the row holding its value there and is appended after the last row when none does; not with row"
        )]
        key: Option<String>,
    ) -> Result<(), Refusal> {
        let sheet = &self.sheet_key(sheet)?;
        let snap = self
            .snapshot_of(sheet)
            .ok_or_else(|| Refusal::new("no such sheet"))?;
        let shown = crate::cell_actions::shown_cells(&snap);
        let request = crate::records::RecordsRequest {
            records: &records,
            row,
            columns: columns.as_deref(),
            key: key.as_deref(),
        };
        let writes = crate::records::set_records_writes(&snap, &shown, &request)?;
        if writes.is_empty() {
            return Err(Refusal::new("records hold no values to write"));
        }
        self.write(sheet, &writes)
    }

    /// Update or add a row by its key.
    ///
    /// Rows are a collection keyed by the headers in row 1, so a caller addresses a row by what it
    /// holds rather than by a row number that moves.
    #[action(answers, item = sheets, audience = "agents")]
    pub fn upsert_item(
        &mut self,
        cx: &mut ActionCx,
        sheet: &Id,
        #[param(
            "The header values that identify the row, e.g. {\"Name\": \"Ana\"}: matched ignoring case and spacing, numbers by value. The one row that holds them is updated; with none, a row is appended; more than one is refused",
            name = "match"
        )]
        matching: std::collections::BTreeMap<String, serde_json::Value>,
        #[param(
            "The row's cells by header, e.g. {\"Total\": \"=[Price]*[Qty]\"}; a header left out keeps its cell. In a formula [Header] is that column's cell in the same row"
        )]
        value: std::collections::BTreeMap<String, serde_json::Value>,
    ) -> Result<(), Refusal> {
        let sheet = &self.sheet_key(sheet)?;
        let snap = self
            .snapshot_of(sheet)
            .ok_or_else(|| Refusal::new("no such sheet"))?;
        let shown = crate::cell_actions::shown_cells(&snap);
        let (writes, appended) = crate::records::upsert_writes(
            &snap,
            &shown,
            &matching.into_iter().collect(),
            &value.into_iter().collect(),
        )?;
        if writes.is_empty() {
            return Err(Refusal::new("the row holds no values to write"));
        }
        let target = self.sheet_mut(sheet)?;
        // A new row under an id of its own, so two agents appending at once do not write into one row.
        if let Some(row) = appended
            && let Ok(at) = usize::try_from(row)
            && at <= target.cells.rows().len()
        {
            target.cells.insert_row(at, cx.id());
        }
        self.write(sheet, &writes)
    }

    /// Duplicate a sheet.
    #[action(item = sheets)]
    pub fn duplicate_sheet(&mut self, cx: &mut ActionCx, sheet: &Id) -> Result<(), Refusal> {
        let sheet = &self.sheet_key(sheet)?;
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
        let sheet = &self.sheet_key(sheet)?;
        // What read the sheet is bound to it, so it reads `#REF!` from now on: a sheet that
        // later takes its name is not the one those formulas meant.
        self.bind_naming(sheet);
        self.sheets
            .remove(sheet)
            .ok_or_else(|| Refusal::new("no such sheet"))?;
        let gone: Vec<Id> = self
            .comments
            .iter()
            .filter(|(_, comment)| comment.anchor.sheet == *sheet)
            .map(|(id, _)| id.clone())
            .collect();
        for comment in gone {
            self.comments.remove(&comment);
        }
        Ok(())
    }

    /// Set cells from a start cell, row by row.
    #[action(answers, item = sheets, audience = "agents")]
    pub fn set_cells(
        &mut self,
        sheet: &Id,
        #[param("Top-left cell, e.g. A1")] start: String,
        #[param(
            "Rows of values written rightward and down from start; \"=…\" is a formula (one that does not parse is kept, shows its error and is listed in the answer's formula_errors; Other!A1 reads another sheet), \"\" clears, null skips, an ISO date such as \"2026-10-22\" (or \"2026-10-22T09:30\") becomes a real date and a leading apostrophe (\"'007\") marks text"
        )]
        rows: Vec<Vec<serde_json::Value>>,
        #[param(
            "The `seen` a read of this workbook answered, to write against the rows and columns that read showed: when a row, a column or a sheet has moved since, nothing is written and the refusal says so"
        )]
        seen: Option<String>,
    ) -> Result<(), Refusal> {
        self.as_seen(seen.as_deref())?;
        let sheet = &self.sheet_key(sheet)?;
        let cells = row_writes(cell_of(&start)?, &rows)?;
        if cells.is_empty() {
            return Err(Refusal::new("rows hold no values to write"));
        }
        self.write(sheet, &cells)
    }

    /// Write cells: values, formulas, formats and bold.
    #[action(answers, item = sheets, audience = "agents")]
    pub fn write_cells(
        &mut self,
        sheet: &Id,
        #[param("The cells to write; what a cell leaves out stays as it is")] cells: Vec<CellWrite>,
        #[param(
            "The `seen` a read of this workbook answered, to write against the rows and columns that read showed: when a row, a column or a sheet has moved since, nothing is written and the refusal says so"
        )]
        seen: Option<String>,
    ) -> Result<(), Refusal> {
        self.as_seen(seen.as_deref())?;
        if cells.is_empty() {
            return Err(Refusal::new("name at least one cell"));
        }
        let sheet = &self.sheet_key(sheet)?;
        let cells = cells
            .into_iter()
            .map(typed_write)
            .collect::<Result<Vec<_>, _>>()?;
        self.write(sheet, &cells)
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
        let sheet = &self.sheet_key(sheet)?;
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
        self.write(sheet, &cells)
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
        let sheet = &self.sheet_key(sheet)?;
        let range = range_of(&range)?;
        let by = letters_to_col(by.trim())
            .filter(|by| (range.0.0..=range.1.0).contains(by))
            .ok_or_else(|| Refusal::new("sort by a column letter inside the range"))?;
        let snap = self
            .snapshot_of(sheet)
            .ok_or_else(|| Refusal::new("no such sheet"))?;
        let shown = crate::cell_actions::shown_cells(&snap);
        let writes =
            crate::cell_actions::sort_writes(&shown, range, by, descending.unwrap_or(false));
        self.write(sheet, &writes)
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
        let sheet = &self.sheet_key(sheet)?;
        let at = at
            .checked_sub(1)
            .filter(|at| *at <= MAX_ROW)
            .ok_or_else(|| Refusal::new("rows are numbered from 1"))?;
        let count = count.unwrap_or(1).clamp(1, 1000);
        self.sheet_mut(sheet)?.reach((0, at));
        let reshape = Reshape::of(self, sheet);
        for _ in 0..count {
            reshape.apply(self, GridEdit::InsertRow(at))?;
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
        let sheet = &self.sheet_key(sheet)?;
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
        let reshape = Reshape::of(self, sheet);
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
            let layout = reshape.apply(self, GridEdit::DeleteRow(at))?;
            if let Some(id) = layout.rows().get(at as usize) {
                let held = self.sheet_mut(sheet)?;
                if layout.is_last_row(id) {
                    held.cut.insert(id.clone());
                }
                held.cells.remove_row(id);
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
        let sheet = &self.sheet_key(sheet)?;
        let at = letters_to_col(at.trim())
            .filter(|at| *at <= MAX_COLUMN)
            .ok_or_else(|| Refusal::new("name a column by its letter, e.g. C"))?;
        let count = count.unwrap_or(1).clamp(1, 1000);
        self.sheet_mut(sheet)?.reach((at, 0));
        let reshape = Reshape::of(self, sheet);
        for _ in 0..count {
            reshape.apply(self, GridEdit::InsertColumn(at))?;
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
        let sheet = &self.sheet_key(sheet)?;
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
        let reshape = Reshape::of(self, sheet);
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
            let layout = reshape.apply(self, GridEdit::DeleteColumn(at))?;
            if let Some(id) = layout.columns().get(at as usize) {
                let held = self.sheet_mut(sheet)?;
                if layout.is_last_column(id) {
                    held.cut.insert(id.clone());
                }
                held.cells.remove_column(id);
            }
        }
        Ok(())
    }

    /// Import sheets.
    ///
    /// Adds the sheets of a file as new sheets, in one step; into a new,
    /// still blank workbook they replace its empty first sheet.
    #[action(audience = "agents")]
    pub fn import_sheets(
        &mut self,
        cx: &mut ActionCx,
        #[param("The sheets, each with its name and cells")] sheets: Vec<ImportedSheet>,
        #[param("The file they come from, which opening again reveals")] source: Option<String>,
        #[param("The workbook's title, e.g. the file's name, when it has none yet")] title: Option<
            String,
        >,
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
        let mut made = Vec::new();
        for imported in sheets {
            let mut sheet = Sheet::named(imported.name.trim());
            sheet.source = source.clone().map(Atom);
            sheet.hidden = imported.hidden;
            write_into(&mut sheet, &imported.cells)?;
            made.push(self.sheets.push(cx.id(), sheet));
        }
        if let Some(blank) = blank {
            self.sheets.remove(&blank);
        }
        self.bind_sheets(&made);
        if let Some(title) = title.as_deref().map(str::trim)
            && !title.is_empty()
            && self.title.is_none()
        {
            self.title = Some(title.to_owned());
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
        let sheet = &self.sheet_key(sheet)?;
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
        let sheet = &self.sheet_key(sheet)?;
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
    #[action(destructive, item = sheets, label = "sheets-delete-chart")]
    pub fn delete_chart(
        &mut self,
        sheet: &Id,
        #[param("The chart")] chart: Id,
    ) -> Result<(), Refusal> {
        let sheet = &self.sheet_key(sheet)?;
        self.sheet_mut(sheet)?
            .charts
            .remove(&chart)
            .map(drop)
            .ok_or_else(|| Refusal::new("no such chart"))
    }

    /// Rename a sheet; the formulas that read it follow it to its new name.
    #[action(item = sheets)]
    pub fn rename_sheet(
        &mut self,
        sheet: &Id,
        #[param("The new name")] name: NonBlank,
    ) -> Result<(), Refusal> {
        let sheet = &self.sheet_key(sheet)?;
        // A formula reads the sheet by its id and spells whatever it is called: one written
        // before references were bound gains its binding first.
        self.bind_naming(sheet);
        self.sheet_mut(sheet)?.name = Atom(name.as_str().to_owned());
        Ok(())
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
        let sheet = &self.sheet_key(sheet)?;
        let (start, end) = range_of(&range)?;
        let text = annotation(&text)?;
        let id = cx.id();
        let area = self
            .sheet_mut(sheet)?
            .area(start, end)
            .ok_or_else(|| Refusal::new("the range is outside the sheet"))?;
        let cells = CommentCells {
            sheet: sheet.clone(),
            area,
        };
        self.comments.push(id, Comment::written(cx, cells, text));
        Ok(())
    }

    /// Reply to a comment.
    #[action(item = comments, label = "sheets-reply-comment")]
    pub fn reply(
        &mut self,
        cx: &mut ActionCx,
        comment: &Id,
        #[param("Reply")] text: String,
    ) -> Result<(), Refusal> {
        let text = annotation(&text)?;
        let id = cx.id();
        let reply = Reply::written(cx, text);
        self.comments
            .get_mut(comment)
            .ok_or_else(|| Refusal::new("the comment no longer exists"))?
            .replies
            .push(id, reply);
        Ok(())
    }

    /// Edit a comment's text.
    #[action(item = comments, label = "sheets-edit-comment")]
    pub fn edit_comment(
        &mut self,
        comment: &Id,
        #[param("Comment")] body: String,
    ) -> Result<(), Refusal> {
        let body = annotation(&body)?;
        self.comments
            .get_mut(comment)
            .ok_or_else(|| Refusal::new("the comment no longer exists"))?
            .text = body;
        Ok(())
    }

    /// Delete a comment.
    #[action(destructive, item = comments, label = "sheets-delete-comment")]
    pub fn delete_comment(&mut self, comment: &Id) -> Result<(), Refusal> {
        self.comments
            .remove(comment)
            .map(drop)
            .ok_or_else(|| Refusal::new("the comment no longer exists"))
    }
}

impl Workbook {
    /// The charts and comment threads of every sheet, as places that open where they are.
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

    /// Every sheet by name, and every cell that shows something by reference, input and
    /// value: the cells a formula spills into among them, which hold no input of their own.
    fn search(&self) -> Vec<SearchEntry> {
        let mut entries = Vec::new();
        for (id, sheet) in self.sheets.iter() {
            entries.push(
                SearchEntry::new(sheet.name.0.clone(), sheet.name.0.clone())
                    .anchored(tilo_resource::items::item_resource_id::<Self>("sheets", id)),
            );
            let Some(snap) = self.snapshot_of(id) else {
                continue;
            };
            for cell in crate::cell_actions::shown_cells_and_spills(&snap) {
                let reference = addr_to_a1(cell.col, cell.row);
                let title = format!("{}!{reference}", sheet.name.0);
                let text = if cell.raw.is_empty() {
                    format!("{reference} {title} {}", cell.display)
                } else {
                    format!("{reference} {title} {} {}", cell.raw, cell.display)
                };
                entries.push(
                    SearchEntry::new(title, text).anchored(cell_anchor(id, (cell.col, cell.row))),
                );
            }
        }
        entries
    }
}

/// The most cells one read answers with; a larger range is read in parts.
pub const MAX_READ_CELLS: usize = 10_000;

impl Workbook {
    /// The workbook's sheets by id and name, with its title.
    fn sheet_listing(&self) -> serde_json::Value {
        let mut listing = serde_json::json!({
            "sheets": self.sheets.iter().map(|(id, sheet)| {
                let layout = sheet.layout();
                let mut entry = serde_json::json!({
                    "item": id.as_str(),
                    "name": sheet.name.0,
                    "rows": layout.rows().len(),
                    "columns": layout.columns().len(),
                });
                if sheet.hidden
                    && let Some(entry) = entry.as_object_mut()
                {
                    entry.insert("hidden".into(), true.into());
                }
                entry
            }).collect::<Vec<_>>(),
            "how": "read an item: a sheet's id or name, with a range after `!` to read part of it; verbs taking a sheet take its name too",
        });
        if let (Some(title), Some(fields)) = (&self.title, listing.as_object_mut()) {
            fields.insert("title".into(), title.clone().into());
        }
        listing
    }

    /// What the workbook holds, for a caller with no screen: its sheets, or one sheet narrowed to a
    /// range, with what each cell shows and the formula behind it.
    fn read_cells(&self, item: Option<&Id>) -> Result<serde_json::Value, String> {
        let Some(item) = item else {
            return Ok(self.sheet_listing());
        };
        if let Some(place) = annotation_place(item.as_str()) {
            return self.read_annotation(place);
        }
        let (sheet_part, range) = item
            .as_str()
            .rsplit_once('!')
            .map_or((item.as_str(), None), |(sheet, range)| (sheet, Some(range)));
        let sheet_key = tilo_resource::items::item_of_resource::<Self>(sheet_part)
            .map_or_else(|| Id::from_raw(sheet_part), |(_, id)| id);
        let (id, sheet) = self
            .sheets
            .iter()
            .find(|(id, sheet)| {
                **id == sheet_key || sheet.name.0.eq_ignore_ascii_case(sheet_part.trim())
            })
            .ok_or_else(|| format!("no sheet `{sheet_part}`; read with no item for the sheets"))?;
        let (top_left, bottom_right, cells) = if let Some(range) = range {
            // A range reads the cells it names alone: the sheet's size is no part of its cost.
            let (start, end) = read_range(range)?;
            let width = usize::try_from(end.0 - start.0 + 1).unwrap_or(usize::MAX);
            let most_rows = u32::try_from((MAX_READ_CELLS / width.max(1)).max(1)).unwrap_or(1);
            let last_row = end.1.min(start.1.saturating_add(most_rows - 1));
            let (snap, values) = crate::cell_actions::cells_of(self, id, start, (end.0, last_row));
            let cells =
                crate::cell_actions::cells_between(&snap, &values, start, (end.0, last_row));
            (start, end, cells)
        } else {
            let snap = self
                .snapshot_of(id)
                .ok_or_else(|| format!("no sheet `{sheet_part}`"))?;
            let cells = crate::cell_actions::shown_cells_and_spills(&snap);
            if cells.is_empty() {
                return Ok(serde_json::json!({
                    "sheet": sheet.name.0,
                    "item": id.as_str(),
                    "rows": [],
                    "seen": self.seen(),
                }));
            }
            let bounds = cells
                .iter()
                .fold(((u32::MAX, u32::MAX), (0, 0)), |(lo, hi), cell| {
                    (
                        (lo.0.min(cell.col), lo.1.min(cell.row)),
                        (hi.0.max(cell.col), hi.1.max(cell.row)),
                    )
                });
            (bounds.0, bounds.1, cells)
        };
        Ok(self.cells_answer(id, sheet, top_left, bottom_right, &cells))
    }

    /// The cells of sheet `sheet` from `top_left` to `bottom_right`, as a read answers them: at
    /// most [`MAX_READ_CELLS`], `next` naming the rest.
    fn cells_answer(
        &self,
        id: &Id,
        sheet: &Sheet,
        top_left: Addr,
        bottom_right: Addr,
        cells: &[crate::cell_actions::SearchableCell],
    ) -> serde_json::Value {
        let width = usize::try_from(bottom_right.0 - top_left.0 + 1).unwrap_or(usize::MAX);
        let most_rows = u32::try_from((MAX_READ_CELLS / width.max(1)).max(1)).unwrap_or(1);
        let last_row = bottom_right.1.min(top_left.1.saturating_add(most_rows - 1));
        let mut rows = vec![
            vec![String::new(); width];
            usize::try_from(last_row - top_left.1 + 1).unwrap_or(0)
        ];
        let mut formulas = serde_json::Map::new();
        for cell in cells {
            if cell.col < top_left.0
                || cell.col > bottom_right.0
                || cell.row < top_left.1
                || cell.row > last_row
            {
                continue;
            }
            let (Ok(row), Ok(col)) = (
                usize::try_from(cell.row - top_left.1),
                usize::try_from(cell.col - top_left.0),
            ) else {
                continue;
            };
            if let Some(slot) = rows.get_mut(row).and_then(|row| row.get_mut(col)) {
                slot.clone_from(&cell.display);
            }
            if cell.raw.starts_with('=') {
                formulas.insert(addr_to_a1(cell.col, cell.row), cell.raw.clone().into());
            }
        }
        let mut answer = serde_json::json!({
            "sheet": sheet.name.0,
            "item": id.as_str(),
            "range": format!(
                "{}:{}",
                addr_to_a1(top_left.0, top_left.1),
                addr_to_a1(bottom_right.0, last_row)
            ),
            "rows": rows,
            "formulas": formulas,
            "seen": self.seen(),
        });
        if last_row < bottom_right.1
            && let Some(map) = answer.as_object_mut()
        {
            map.insert(
                "next".into(),
                format!(
                    "{}!{}:{}",
                    sheet.name.0,
                    addr_to_a1(top_left.0, last_row + 1),
                    addr_to_a1(bottom_right.0, bottom_right.1)
                )
                .into(),
            );
        }
        answer
    }
}

/// A chart, comment thread or reply, by the resource it is published as.
#[derive(Clone, Copy)]
enum Annotation<'a> {
    Chart(&'a str),
    Comment(&'a str),
    Reply(&'a str, &'a str),
}

/// The chart, thread or reply the published id names.
fn annotation_place(id: &str) -> Option<Annotation<'_>> {
    let rest = id
        .strip_prefix(<Workbook as tilo_resource::Resource>::KIND)?
        .strip_prefix('/')?;
    if let Some(place) = rest.strip_prefix("derived/") {
        return place
            .strip_prefix("chart:")
            .map(Annotation::Chart)
            .or_else(|| place.strip_prefix("comment:").map(Annotation::Comment));
    }
    match rest.split('/').collect::<Vec<_>>()[..] {
        ["sheets", sheet, "charts", chart] if !sheet.is_empty() && !chart.is_empty() => {
            Some(Annotation::Chart(chart))
        }
        ["comments", comment] if !comment.is_empty() => Some(Annotation::Comment(comment)),
        ["comments", comment, "replies", reply] if !comment.is_empty() && !reply.is_empty() => {
            Some(Annotation::Reply(comment, reply))
        }
        _ => None,
    }
}

/// Whether the id names a sheet.
fn is_sheet_item(id: &str) -> bool {
    id.strip_prefix(<Workbook as tilo_resource::Resource>::KIND)
        .and_then(|rest| rest.strip_prefix("/sheets/"))
        .is_some_and(|sheet| !sheet.is_empty() && !sheet.contains('/'))
}

impl Workbook {
    /// Whether `id` is part of what the read answers, offered to whoever may read it.
    fn document_child(_: &Self, id: &str) -> bool {
        is_sheet_item(id) || annotation_place(id).is_some()
    }

    /// What a chart, thread or reply is: its sheet and cells, and its own content.
    fn read_annotation(&self, place: Annotation<'_>) -> Result<serde_json::Value, String> {
        for (sheet_id, sheet) in self.sheets.iter() {
            let layout = sheet.layout();
            if let Annotation::Chart(id) = place {
                if let Some(chart) = sheet
                    .chart_snapshots(&layout)
                    .into_iter()
                    .find(|chart| chart.id == id)
                {
                    return Ok(serde_json::json!({
                        "kind": "chart",
                        "chart": chart.id,
                        "title": chart.title,
                        "type": chart.kind.code(),
                        "sheet": sheet.name.0,
                        "range": range_label(chart.start, chart.end),
                        "how": "read the sheet with this range for the values it charts",
                    }));
                }
                continue;
            }
            let (Annotation::Comment(comment) | Annotation::Reply(comment, _)) = place else {
                continue;
            };
            let Some(thread) = Sheet::comment_snapshots(sheet_id, &self.comments, &layout)
                .into_iter()
                .find(|thread| thread.id == comment)
            else {
                continue;
            };
            if let Annotation::Reply(_, reply) = place {
                let reply = thread
                    .replies
                    .iter()
                    .find(|candidate| candidate.id == reply)
                    .ok_or_else(|| {
                        format!("no reply `{reply}`; read the comment for its replies")
                    })?;
                return Ok(serde_json::json!({
                    "kind": "reply",
                    "comment": thread.id,
                    "author": reply.author,
                    "body": reply.body,
                }));
            }
            return Ok(serde_json::json!({
                "kind": "comment",
                "comment": thread.id,
                "sheet": sheet.name.0,
                "range": range_label(thread.start, thread.end),
                "author": thread.author,
                "body": thread.body,
                "resolved": thread.resolved,
                "replies": thread.replies.iter().map(|reply| serde_json::json!({
                    "author": reply.author,
                    "body": reply.body,
                })).collect::<Vec<_>>(),
            }));
        }
        Err(match place {
            Annotation::Chart(id) => format!("no chart `{id}`; read with no item for the sheets"),
            Annotation::Comment(id) | Annotation::Reply(id, _) => {
                format!("no comment `{id}`; read with no item for the sheets")
            }
        })
    }
}

/// The corners of an A1 range, or of one cell.
fn read_range(range: &str) -> Result<(Addr, Addr), String> {
    let refused = || format!("`{range}` is not a range like A1:C10");
    let (start, end) = range.split_once(':').unwrap_or((range, range));
    let start = a1_to_addr(start).ok_or_else(refused)?;
    let end = a1_to_addr(end).ok_or_else(refused)?;
    Ok((
        (start.0.min(end.0), start.1.min(end.1)),
        (start.0.max(end.0), start.1.max(end.1)),
    ))
}

/// The search anchor of a cell.
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

/// Framework app: the sheets grid is the view of [`Workbook`].
pub struct SheetsShell;

/// What the binary runs instead of opening a window: `--probe-spreadsheet-engine`
/// runs the formula engine's self-check, which proves Sheets loads the engine
/// library and runs it. It needs `runtime-engine`.
const COMMANDS: &[ui::framework::Command] = &[ui::framework::Command {
    option: "--probe-spreadsheet-engine",
    run: probe_spreadsheet_engine,
}];

#[cfg(feature = "runtime-engine")]
fn probe_spreadsheet_engine(_: &str) -> Result<(), String> {
    let report = tilo_spreadsheet_engine::self_check()?;
    println!("sheets --probe-spreadsheet-engine: {report}");
    Ok(())
}

#[cfg(not(feature = "runtime-engine"))]
fn probe_spreadsheet_engine(_: &str) -> Result<(), String> {
    Err("the spreadsheet engine library needs the `runtime-engine` feature".to_string())
}

/// Every capture scenario: the sample workbook with one surface in view.
#[cfg(feature = "samples")]
const SCENARIOS: &[&str] = &[
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
    "find",
    "replace",
    "cellmenu",
];

/// When the sample workbook of the annotated scenarios was written.
#[cfg(feature = "samples")]
const SAMPLE_WRITTEN_MS: i64 = 1_767_225_600_000;

/// Whether the scenario shows the sample's charts and comments.
#[cfg(feature = "samples")]
fn annotated(scenario: Option<&str>) -> bool {
    matches!(scenario, Some("charts" | "comments"))
}

impl ResourceApp for SheetsShell {
    type Resource = Workbook;
    const ID: &'static str = ui::apps::SHEETS;
    // Each window shows the document it opened, never another window's.
    const WINDOW_RESOURCE: bool = true;
    const TITLE: &'static str = "Sheets";
    const TITLE_KEY: &'static str = "sheets-app-title";
    const ICON: Option<&'static str> = Some("sheets");

    fn commands() -> &'static [ui::framework::Command] {
        COMMANDS
    }

    #[cfg(feature = "samples")]
    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&CATALOG)
    }

    /// The workbook's title, as the palette and history name it.
    fn label(state: &Workbook) -> String {
        state
            .title
            .clone()
            .unwrap_or_else(|| <Workbook as ui::tilo_resource::Resource>::LABEL.to_owned())
    }

    /// The annotated scenarios show a workbook other people wrote: Alex shared it and commented, Sam
    /// replied.
    #[cfg(feature = "samples")]
    fn fixture(fixture: &mut Fixture<Workbook>) {
        if !annotated(fixture.scenario()) {
            return;
        }
        let versions = crate::sample::workbook()
            .into_iter()
            .map(|invoke| ("Alex", invoke))
            .chain(crate::sample::comments());
        for (at, (author, invoke)) in (0_i64..).zip(versions) {
            fixture.version(
                ui::tilo_resource::MemberId::person(author),
                SAMPLE_WRITTEN_MS + at * 60_000,
                invoke,
            );
        }
    }

    #[cfg(feature = "samples")]
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

    /// The find shots are the window bar's search: its field with a query, driving the sheet's
    /// matches. Replace opens the search on its replacement.
    #[cfg(feature = "samples")]
    fn scenario_input(
        name: Option<&str>,
        input: &mut ui::headless::FrameHarness,
    ) -> Result<(), String> {
        match name {
            Some("find") => {
                input.search_in_window(ui::window_toolbar::SearchOpening::Find, "a");
            }
            Some("replace") => {
                input.search_in_window(ui::window_toolbar::SearchOpening::Replace, "Total");
                input.step_window_search(true);
            }
            _ => {}
        }
        Ok(())
    }

    fn view(cx: &mut ViewCx<Workbook>) -> Element {
        crate::app::window(cx, SheetsApp::new())
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod agent_tests;

#[cfg(test)]
mod reference_tests;

#[cfg(test)]
mod binding_tests;

#[cfg(test)]
mod concurrent_tests;

#[cfg(test)]
mod cost_tests;

ui::reach_tests!(
    SheetsShell,
    shares: [share_0, share_1, share_2, share_3],
);

#[cfg(test)]
mod agent_disclosure_tests {
    use ui::framework::ResourceApp;
    use ui::framework::testing::TestApp;
    use ui::serde_json::Value;
    use ui::tilo_actions::ResourceSpec;

    /// `scenario` as a drive stages it, settled, and what it publishes.
    fn driven(scenario: &str) -> (TestApp<super::SheetsShell>, Vec<ResourceSpec>) {
        let mut app = TestApp::<super::SheetsShell>::drive(scenario)
            .unwrap_or_else(|error| panic!("{scenario}: {error}"));
        app.settle();
        let resources = app.harness().resources();
        (app, resources)
    }

    /// What the document read of each of `kinds` answers, in the first scenario that publishes
    /// that kind with one. One walk over the scenarios serves every kind. Each read is asked of
    /// a window of its own, driven and settled and asked nothing before: a window that answered
    /// is dropped, and the next kind its scenario publishes gets the scenario driven again.
    fn read_first_of_each(kinds: &[&str]) -> Vec<Value> {
        let mut answers: Vec<Option<Value>> = vec![None; kinds.len()];
        for scenario in <super::SheetsShell as ResourceApp>::scenarios() {
            if answers.iter().all(Option::is_some) {
                break;
            }
            let mut unasked = Some(driven(scenario));
            let waiting = kinds
                .iter()
                .zip(&mut answers)
                .filter(|(_, answer)| answer.is_none());
            for (kind, answer) in waiting {
                let (mut app, resources) = unasked.take().unwrap_or_else(|| driven(scenario));
                match document_read(&resources, kind) {
                    Some((resource, read)) => {
                        let answered = app
                            .call_on(&resource, &read, ui::serde_json::json!({}))
                            .unwrap_or_else(|error| panic!("{kind} in {scenario}: {error}"))
                            .unwrap_or_else(|| panic!("the read of {kind} answers"));
                        *answer = Some(answered);
                    }
                    None => unasked = Some((app, resources)),
                }
            }
        }
        kinds
            .iter()
            .zip(answers)
            .map(|(kind, answer)| {
                answer.unwrap_or_else(|| {
                    panic!("no scenario publishes a {kind} with a document read")
                })
            })
            .collect()
    }

    fn document_read(resources: &[ResourceSpec], kind: &str) -> Option<(String, String)> {
        resources.iter().find_map(|resource| {
            resource
                .actions
                .iter()
                .find(|action| resource.kind == kind && action.contract.reads_document())
                .map(|read| (resource.id.as_str().to_owned(), read.id.clone()))
                .or_else(|| document_read(&resource.children, kind))
        })
    }

    #[test]
    fn a_chart_a_comment_a_sheet_and_a_comment_item_are_read_through_the_workbook_read() {
        // The kind published, the kind its read says it answers for, and a field the answer
        // carries as text.
        let rows: [(&str, Option<&str>, Option<&str>); 4] = [
            ("sheets.workbook.chart", Some("chart"), Some("range")),
            ("sheets.workbook.comment", Some("comment"), Some("body")),
            ("sheets.workbook.sheets", None, Some("sheet")),
            ("sheets.workbook.comments", Some("comment"), None),
        ];
        let kinds: Vec<&str> = rows.iter().map(|row| row.0).collect();
        let answers = read_first_of_each(&kinds);
        for ((kind, says, text), answer) in rows.into_iter().zip(answers) {
            if let Some(says) = says {
                assert_eq!(answer["kind"], says, "{kind}: {answer}");
            }
            if let Some(text) = text {
                assert!(answer[text].is_string(), "{kind} `{text}`: {answer}");
            }
        }
    }
}
