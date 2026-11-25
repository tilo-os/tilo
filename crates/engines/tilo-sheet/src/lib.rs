//! The spreadsheet engine: a workbook's cells as typed, the values they compute, the formats they
//! show in, smart fill, and the interchange its files hold. It draws nothing and knows no window:
//! a tilolang app hands it the cells a person edited and shows what it answers.
//!
//! Everything crosses this boundary as JSON, the shape the app's types have: a sheet is its name,
//! whether its tab is hidden, the raw input and the format of each cell by its A1 reference, and
//! the cells shown bold.

mod calc;
mod csv;
pub mod edit;
pub mod fill;
pub mod infer;
pub mod model;
pub mod notion;
mod ods;
mod records;
mod refs;
mod xlsx;

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

pub use edit::Change;
pub use model::{Addr, CellFormat, CellValue, FormulaError, a1_to_addr, addr_to_a1};

/// How a workbook is written to a file, by the file's kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Xlsx,
    Ods,
    Csv,
    Tsv,
}

impl Format {
    /// The format a file name or media type names, by its ending.
    #[must_use]
    pub fn of_name(name: &str) -> Option<Self> {
        let lower = name.to_ascii_lowercase();
        let ending = lower.rsplit('.').next().unwrap_or_default();
        Some(match ending {
            "xlsx" => Self::Xlsx,
            "ods" => Self::Ods,
            "csv" | "zip" | "notion" => Self::Csv,
            "tsv" | "tab" => Self::Tsv,
            _ => {
                let media = lower
                    .split(';')
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_owned();
                match media.as_str() {
                    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => {
                        Self::Xlsx
                    }
                    "application/vnd.oasis.opendocument.spreadsheet" => Self::Ods,
                    "text/csv" => Self::Csv,
                    "text/tab-separated-values" => Self::Tsv,
                    _ => return None,
                }
            }
        })
    }
}

/// One sheet: the tab it shows, and its cells as typed, their formats and their weight.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sheet {
    pub name: String,
    #[serde(default)]
    pub hidden: bool,
    /// Each cell's raw input as typed, by its A1 reference.
    #[serde(default)]
    pub cells: BTreeMap<String, String>,
    /// Each formatted cell's format code, by its A1 reference.
    #[serde(default)]
    pub formats: BTreeMap<String, String>,
    /// The cells shown bold.
    #[serde(default)]
    pub bold: Vec<String>,
}

impl Sheet {
    /// The extent of the cells that hold anything: how many columns and rows a window shows at
    /// least, counting from the top left.
    #[must_use]
    pub fn extent(&self) -> (u32, u32) {
        let mut columns = 0;
        let mut rows = 0;
        let references = self
            .cells
            .keys()
            .chain(self.formats.keys())
            .chain(self.bold.iter());
        for reference in references {
            if let Some((col, row)) = a1_to_addr(reference) {
                columns = columns.max(col.saturating_add(1));
                rows = rows.max(row.saturating_add(1));
            }
        }
        (columns, rows)
    }
}

/// The workbook a call reads: its sheets and the currency a bare money format shows in.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Book {
    #[serde(default)]
    pub sheets: Vec<Sheet>,
    /// The currency the workbook writes bare money in, by its ISO code.
    #[serde(default)]
    pub currency: String,
    /// The language its cells' own date and time formats read in (`en`, `de`): the workbook's,
    /// not the reader's, so every member of the workspace reads a value alike.
    #[serde(default)]
    pub language: String,
}

impl Book {
    /// The currency a `currency` (no code) format shows in.
    #[must_use]
    pub fn currency(&self) -> tilo_formats::Currency {
        tilo_formats::Currency::from_code(&self.currency).unwrap_or(tilo_formats::Currency::USD)
    }

    /// The words its cells' formats are said in.
    #[must_use]
    pub fn words(&self) -> model::LanguageWords {
        model::LanguageWords::of(&self.language)
    }
}

/// A sheet's cells as they show, with the extent its window has to hold.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ShownSheet {
    pub name: String,
    pub columns: u32,
    pub rows: u32,
    pub cells: BTreeMap<String, CellShown>,
}

/// A shown cell with the format and weight it stands in.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CellShown {
    pub shown: String,
    pub raw: String,
    pub kind: String,
    pub format: String,
}

/// What each cell of every sheet shows, in the order the sheets stand.
///
/// # Errors
/// Never for a well-formed book; a sheet whose name the engine refuses is left empty.
pub fn values(book: &Book) -> Vec<ShownSheet> {
    let words = &book.words();
    let inputs: Vec<(String, std::collections::HashMap<Addr, String>)> = book
        .sheets
        .iter()
        .map(|sheet| (sheet.name.clone(), sheet.inputs()))
        .collect();
    book.sheets
        .iter()
        .zip(0usize..)
        .map(|(sheet, own)| {
            let computed = calc::evaluate(&inputs, own);
            let formats: std::collections::HashMap<Addr, CellFormat> = sheet
                .formats
                .iter()
                .filter_map(|(reference, code)| {
                    Some((a1_to_addr(reference)?, CellFormat::from_str(code)))
                })
                .collect();
            let mut shown = ShownSheet {
                name: sheet.name.clone(),
                ..ShownSheet::default()
            };
            let (columns, rows) = sheet.extent();
            shown.columns = columns;
            shown.rows = rows;
            for (reference, raw) in &sheet.cells {
                let Some(addr) = a1_to_addr(reference) else {
                    continue;
                };
                let value = computed.get(&addr).cloned().unwrap_or(CellValue::Empty);
                let format = formats.get(&addr).copied().unwrap_or(CellFormat::General);
                let kind = match &value {
                    CellValue::Empty => {
                        if raw.trim().is_empty() {
                            continue;
                        }
                        "text"
                    }
                    CellValue::Number(_) => "number",
                    CellValue::Text(_) => "text",
                    CellValue::Bool(_) => "bool",
                    CellValue::Error(_) => "error",
                };
                let shown_text = if kind == "text" && value.is_empty() {
                    raw.clone()
                } else {
                    model::format_value(&value, format, words)
                };
                shown.cells.insert(
                    reference.clone(),
                    CellShown {
                        shown: shown_text,
                        raw: raw.clone(),
                        kind: kind.to_owned(),
                        format: format.as_str(),
                    },
                );
            }
            // A cell formatted but left empty still shows its format: a money column typed into
            // later keeps its shape.
            for (reference, code) in &sheet.formats {
                if shown.cells.contains_key(reference) {
                    continue;
                }
                if let Some(addr) = a1_to_addr(reference) {
                    shown.cells.insert(
                        reference.clone(),
                        CellShown {
                            shown: String::new(),
                            raw: String::new(),
                            kind: match computed.get(&addr) {
                                Some(CellValue::Number(_)) => "number".into(),
                                _ => "empty".into(),
                            },
                            format: CellFormat::from_str(code).as_str(),
                        },
                    );
                }
            }
            for reference in &sheet.bold {
                if let Some(cell) = shown.cells.get_mut(reference) {
                    cell.format.push_str(":bold");
                }
            }
            let (columns, rows) = sheet.extent();
            shown.columns = columns.max(1);
            shown.rows = rows.max(1);
            shown
        })
        .collect()
}

/// The input to store for a typed entry, and the format code it shows in, with the currency a
/// bare money symbol means named by its ISO code.
#[must_use]
pub fn entry_of(input: &str, currency: &str) -> (String, String) {
    let currency =
        tilo_formats::Currency::from_code(currency).unwrap_or(tilo_formats::Currency::USD);
    let inferred = infer::infer_entry(input, currency);
    (inferred.raw, inferred.format.as_str())
}

/// Each sheet as an app reads it to draw its grid: its name, how many columns and rows it shows,
/// and then each cell that shows anything as its reference, what it shows and how it reads
/// (`text`, `number`, `bool` or `error`).
///
/// A call answers a list of sheets, each a list of rows that say what they are: the sheet's own
/// name and extent, tagged `sheet`, and one `cell` row for each cell that shows anything.
#[must_use]
pub fn shown_rows(book: &Book) -> Vec<Vec<Vec<String>>> {
    values(book)
        .into_iter()
        .map(|sheet| {
            let mut rows = vec![vec![
                "sheet".to_owned(),
                sheet.name,
                sheet.columns.to_string(),
                sheet.rows.to_string(),
            ]];
            for (reference, cell) in sheet.cells {
                rows.push(vec!["cell".to_owned(), reference, cell.shown, cell.kind]);
            }
            rows
        })
        .collect()
}

/// The sheets as an app keeps them: each sheet's name and whether its tab is hidden, and then
/// each cell that holds anything as its reference, the input typed into it, its format code and
/// whether it is bold.
#[must_use]
pub fn sheet_rows(sheets: &[Sheet]) -> Vec<Vec<Vec<String>>> {
    sheets
        .iter()
        .map(|sheet| {
            let mut rows = vec![vec![
                "sheet".to_owned(),
                sheet.name.clone(),
                if sheet.hidden { "1" } else { "0" }.to_owned(),
            ]];
            let mut references: BTreeSet<&str> = sheet.cells.keys().map(String::as_str).collect();
            references.extend(sheet.formats.keys().map(String::as_str));
            references.extend(sheet.bold.iter().map(String::as_str));
            for reference in references {
                rows.push(vec![
                    "cell".to_owned(),
                    reference.to_owned(),
                    sheet.cells.get(reference).cloned().unwrap_or_default(),
                    sheet.formats.get(reference).cloned().unwrap_or_default(),
                    if sheet.bold.iter().any(|bold| bold == reference) {
                        "1"
                    } else {
                        "0"
                    }
                    .to_owned(),
                ]);
            }
            rows
        })
        .collect()
}

/// What a workbook's file holds, each sheet with its cells, formats and bold rows.
///
/// # Errors
/// The bytes are not a spreadsheet of that kind, or the format is one this engine cannot read.
pub fn import(
    bytes: &[u8],
    name: &str,
    notion_words: &notion::Words,
) -> Result<Vec<Sheet>, String> {
    let format = Format::of_name(name).ok_or_else(|| format!("`{name}` is not a spreadsheet"))?;
    let is_notion = name.to_ascii_lowercase().ends_with(".zip");
    if is_notion {
        let tables = notion::tables(bytes, notion_words)?;
        return tables
            .into_iter()
            .map(|table| {
                let cells = csv::parse_separated(&table.csv, '\t');
                Ok(sheet_of(
                    table.name,
                    cells,
                    std::collections::HashMap::new(),
                ))
            })
            .collect();
    }
    match format {
        Format::Csv | Format::Tsv => {
            let text =
                std::str::from_utf8(bytes).map_err(|_| "the table is not text".to_owned())?;
            let separator = if format == Format::Tsv { '\t' } else { ',' };
            let cells = csv::parse_separated(text, separator);
            let name = sheet_name(name);
            Ok(vec![sheet_of(
                name,
                cells,
                std::collections::HashMap::new(),
            )])
        }
        Format::Xlsx => {
            let tables = xlsx::parse_xlsx(bytes).map_err(|error| error.to_string())?;
            Ok(tables.into_iter().map(sheet_of_table).collect())
        }
        Format::Ods => {
            let tables = ods::parse_ods(bytes).map_err(|error| error.to_string())?;
            Ok(tables.into_iter().map(sheet_of_table).collect())
        }
    }
}

/// Write `book`'s sheets as a file of `format`; CSV and TSV write the first sheet, as people
/// export one sheet.
///
/// # Errors
/// The workbook cannot be written as that kind, which is never for the formats an app offers.
pub fn export(book: &Book, format: Format) -> Result<Vec<u8>, String> {
    match format {
        Format::Xlsx => xlsx::xlsx_bytes(&tables(book)).map_err(|error| error.to_string()),
        Format::Ods => ods::ods_bytes(&tables(book)).map_err(|error| error.to_string()),
        Format::Csv | Format::Tsv => {
            let separator = if format == Format::Tsv { '\t' } else { ',' };
            Ok(csv_of(book, separator).into_bytes())
        }
    }
}

/// What a sheet's cells show as rows of text, the form CSV and TSV write.
fn csv_of(book: &Book, separator: char) -> String {
    let shown = values(book);
    let Some(sheet) = shown.first() else {
        return String::new();
    };
    let mut rows: Vec<Vec<String>> = Vec::new();
    for row in 0..sheet.rows {
        let mut fields = Vec::new();
        for column in 0..sheet.columns {
            let reference = addr_to_a1(column, row);
            let text = sheet
                .cells
                .get(&reference)
                .map_or_else(String::new, |cell| cell.shown.clone());
            fields.push(text);
        }
        rows.push(fields);
    }
    csv::to_separated(&rows, separator)
}

/// The sheets as the interchange codecs read them, with each formula kept as one.
fn tables(book: &Book) -> Vec<xlsx::SheetTable> {
    let shown = values(book);
    book.sheets
        .iter()
        .zip(shown)
        .map(|(sheet, shown)| {
            let mut table = xlsx::SheetTable {
                name: sheet.name.clone(),
                hidden: sheet.hidden,
                rows: Vec::new(),
            };
            for row in 0..shown.rows {
                let mut fields = Vec::new();
                for column in 0..shown.columns {
                    let reference = addr_to_a1(column, row);
                    let cell = sheet.cells.get(&reference);
                    let format = sheet
                        .formats
                        .get(&reference)
                        .map_or(CellFormat::General, |code| CellFormat::from_str(code));
                    let value = shown.cells.get(&reference);
                    fields.push(match (cell, value) {
                        (Some(raw), Some(value)) => {
                            let formula = raw.trim_start().starts_with('=').then(|| raw.clone());
                            Some(xlsx::ImportedCell {
                                raw: if formula.is_some() {
                                    value.shown.clone()
                                } else {
                                    model::format_number_general(
                                        raw.trim().parse::<f64>().unwrap_or(0.0),
                                    )
                                },
                                format,
                                formula,
                                bold: sheet.bold.contains(&reference),
                            })
                        }
                        _ => None,
                    });
                }
                table.rows.push(fields);
            }
            table
        })
        .collect()
}

/// A parsed cell map as the app's sheet.
fn sheet_of(
    name: String,
    cells: std::collections::HashMap<Addr, String>,
    formats: std::collections::HashMap<Addr, CellFormat>,
) -> Sheet {
    let mut sheet = Sheet {
        name,
        ..Sheet::default()
    };
    for (addr, input) in cells {
        if input.is_empty() {
            continue;
        }
        sheet.cells.insert(addr_to_a1(addr.0, addr.1), input);
    }
    for (addr, format) in formats {
        if format == CellFormat::General {
            continue;
        }
        sheet
            .formats
            .insert(addr_to_a1(addr.0, addr.1), format.as_str());
    }
    sheet
}

fn sheet_of_table(table: xlsx::SheetTable) -> Sheet {
    let mut sheet = Sheet {
        name: table.name,
        hidden: table.hidden,
        ..Sheet::default()
    };
    for (row, cells) in table.rows.iter().enumerate() {
        for (column, cell) in cells.iter().enumerate() {
            let Some(cell) = cell else {
                continue;
            };
            let Ok(row) = u32::try_from(row) else {
                continue;
            };
            let Ok(column) = u32::try_from(column) else {
                continue;
            };
            let reference = addr_to_a1(column, row);
            let input = cell.formula.clone().unwrap_or_else(|| cell.raw.clone());
            if !input.is_empty() {
                sheet.cells.insert(reference.clone(), input);
            }
            if cell.format != CellFormat::General {
                sheet
                    .formats
                    .insert(reference.clone(), cell.format.as_str());
            }
            if cell.bold {
                sheet.bold.push(reference);
            }
        }
    }
    sheet
}

/// The name a sheet takes from the file it was read from.
fn sheet_name(file: &str) -> String {
    let bare = file.rsplit(['/', '\\']).next().unwrap_or(file);
    let stem = bare.rsplit_once('.').map_or(bare, |(stem, _)| stem);
    let stem = stem.trim();
    if stem.is_empty() {
        "Sheet1".to_owned()
    } else {
        stem.to_owned()
    }
}

/// Apply a structural change to the workbook's sheets: rows or columns inserted or removed, a
/// range sorted, or a fill extended, with the formulas the change moves rewritten.
///
/// # Errors
/// The change names a sheet or a range the workbook does not hold.
pub fn edit(book: &Book, change: &edit::Change) -> Result<Vec<Sheet>, String> {
    edit::apply(book, change)
}

/// The numbers a range of a sheet shows, row by row: a chart's readings. A cell that is not a
/// number reads as 0, so every row of the range has its reading.
#[must_use]
pub fn range_numbers(book: &Book, sheet: &str, range: &str) -> Vec<f64> {
    let Some(own) = book
        .sheets
        .iter()
        .position(|shown| refs::fold_sheet(&shown.name) == refs::fold_sheet(sheet))
    else {
        return Vec::new();
    };
    let Some((start, end)) = corners_of(range) else {
        return Vec::new();
    };
    let inputs: Vec<(String, std::collections::HashMap<Addr, String>)> = book
        .sheets
        .iter()
        .map(|sheet| (sheet.name.clone(), sheet.inputs()))
        .collect();
    let computed = calc::evaluate(&inputs, own);
    let column = if end.0 > start.0 { end.0 } else { start.0 };
    (start.1..=end.1)
        .map(|row| {
            computed
                .get(&(column, row))
                .and_then(CellValue::as_number)
                .unwrap_or(0.0)
        })
        .collect()
}

/// The two corners of an A1 range, top left first.
fn corners_of(text: &str) -> Option<(Addr, Addr)> {
    let mut parts = text.trim().split(':');
    let first = parts.next().unwrap_or_default();
    let last = parts.next().unwrap_or(first);
    let start = a1_to_addr(first)?;
    let end = a1_to_addr(last)?;
    Some((
        (start.0.min(end.0), start.1.min(end.1)),
        (start.0.max(end.0), start.1.max(end.1)),
    ))
}

/// The ranges of `sheet` as they stand after `change`, each empty where the change removes every
/// cell it names: a chart or a comment keeps the cells it is about as the rows under them move.
#[must_use]
pub fn moved_ranges(change: &edit::Change, sheet: &str, areas: &[String]) -> Vec<String> {
    areas
        .iter()
        .map(|area| edit::moved_range(change, sheet, area).unwrap_or_default())
        .collect()
}

/// The cells a record is written as, for the actions that take records.
pub use records::{RecordsRequest, Writes, set_records, upsert};

#[cfg(test)]
mod tests {
    use super::*;

    fn book() -> Book {
        let mut cells = BTreeMap::new();
        cells.insert("A1".to_owned(), "2".to_owned());
        cells.insert("A2".to_owned(), "3".to_owned());
        cells.insert("A3".to_owned(), "=SUM(A1:A2)".to_owned());
        let mut formats = BTreeMap::new();
        formats.insert("A1".to_owned(), "currency:EUR".to_owned());
        Book {
            sheets: vec![Sheet {
                name: "Sheet1".to_owned(),
                cells,
                formats,
                ..Sheet::default()
            }],
            currency: "USD".to_owned(),
            language: String::new(),
        }
    }

    #[test]
    fn a_sheet_shows_its_computed_cells_in_their_formats() {
        let shown = values(&book());
        let sheet = &shown[0];
        assert_eq!(sheet.rows, 3);
        assert_eq!(sheet.columns, 1);
        assert_eq!(sheet.cells["A3"].shown, "5");
        assert_eq!(sheet.cells["A1"].shown, "2,00\u{00A0}\u{20AC}");
        assert_eq!(sheet.cells["A1"].kind, "number");
    }

    #[test]
    fn a_csv_file_reads_as_one_sheet_and_writes_what_it_shows() {
        let sheets = import(b"a,b\n1,2\n", "t.csv", &notion::Words::default()).unwrap();
        assert_eq!(sheets.len(), 1);
        assert_eq!(sheets[0].name, "t");
        assert_eq!(sheets[0].cells["B2"], "2");
        let mut book = Book {
            sheets,
            currency: String::new(),
            language: String::new(),
        };
        book.sheets[0]
            .cells
            .insert("C1".to_owned(), "=A2+B2".to_owned());
        let bytes = export(&book, Format::Csv).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.starts_with("a,b,3\n"), "{text}");
    }
}
