//! Spreadsheets: a workbook file read as the sheets it holds, a workbook written as a file, the
//! values its cells compute, and the sheets a structural change or a written record leaves. The
//! engine is `tilo-sheet`, which draws nothing.
//!
//! A workbook crosses this boundary as JSON, the shape its resources have: its sheets, each with
//! its name, whether its tab is hidden, and the input, format and weight of its cells by their A1
//! reference. What the engine answers comes back as rows of texts a view reads without a
//! document: a sheet's name, then its cells.

use crate::abi::Imp;
use crate::blob::{BlobHost, FileValue};
use crate::core::{Args, Core, HResult, trap};
use crate::engine::Wasm;
use crate::value::Value;

/// The most a spreadsheet file may hold: an import reads it whole.
const MAX_BYTES: i64 = 64 * 1024 * 1024;

impl Core {
    /// The workbook a text describes, with each sheet's extent and what each of its cells shows.
    pub(crate) fn sheet_values(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let text = self.text(a.h(0)?)?;
        let answer = self.book_of(&text).and_then(|book| {
            let rows = tilo_sheet::shown_rows(&book);
            serde_json::to_string(&rows).map_err(|why| why.to_string())
        });
        self.json_result(answer)
    }

    /// The sheets a structural change leaves: rows and columns inserted or removed, a range
    /// sorted, a fill extended or a sheet renamed, with the formulas the change moves rewritten.
    pub(crate) fn sheet_edit(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let book = self.text(a.h(0)?)?;
        let change = self.text(a.h(1)?)?;
        let answer = self.book_of(&book).and_then(|book: tilo_sheet::Book| {
            let change: tilo_sheet::Change = serde_json::from_str(&change)
                .map_err(|why| format!("the change is not a change of a sheet: {why}"))?;
            let sheets = tilo_sheet::edit(&book, &change)?;
            serde_json::to_string(&tilo_sheet::sheet_rows(&sheets))
                .map_err(|why| why.to_string())
        });
        self.json_result(answer)
    }

    /// The numbers a range of a sheet shows, row by row: a chart's readings.
    pub(crate) fn sheet_numbers(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let book = self.text(a.h(0)?)?;
        let sheet = self.text(a.h(1)?)?;
        let range = self.text(a.h(2)?)?;
        let answer = self.book_of(&book).and_then(|book: tilo_sheet::Book| {
            serde_json::to_string(&tilo_sheet::range_numbers(&book, &sheet, &range))
                .map_err(|why| why.to_string())
        });
        self.json_result(answer)
    }

    /// The ranges of a sheet as a structural change leaves them, each empty where the change
    /// removes every cell it names.
    pub(crate) fn sheet_ranges(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let change = self.text(a.h(0)?)?;
        let sheet = self.text(a.h(1)?)?;
        let areas = match self.val_whole(a.h(2)?)? {
            Value::List(items) => {
                let mut areas = Vec::with_capacity(items.len());
                for item in &items {
                    match item {
                        Value::Text(text) => areas.push(text.clone()),
                        other => {
                            return trap(format!(
                                "the ranges are texts, and this list holds a {}",
                                other.kind_name()
                            ));
                        }
                    }
                }
                areas
            }
            other => {
                return trap(format!(
                    "the ranges are a list, and this is a {}",
                    other.kind_name()
                ));
            }
        };
        let answer = serde_json::from_str::<tilo_sheet::Change>(&change)
            .map_err(|why| format!("the change is not a change of a sheet: {why}"))
            .and_then(|change| {
                serde_json::to_string(&tilo_sheet::moved_ranges(&change, &sheet, &areas))
                    .map_err(|why| why.to_string())
            });
        self.json_result(answer)
    }

    /// The cells a record action writes into a sheet: `set-records` by column header, or
    /// `upsert-item` by the values that identify its row.
    pub(crate) fn sheet_records(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let sheet = self.text(a.h(0)?)?;
        let request = self.text(a.h(1)?)?;
        let answer = sheet_of(&sheet).and_then(|sheet| {
            let request: serde_json::Value = serde_json::from_str(&request)
                .map_err(|why| format!("the records are not JSON: {why}"))?;
            let writes = records_of(&sheet, &request)?;
            writes_json(&writes).map_err(|why| why.to_string())
        });
        self.json_result(answer)
    }

    /// The input to store for a typed entry, and the format it shows in: `90%` as a fraction
    /// shown as a percent, `2024-01-31` as a date, a leading apostrophe as text.
    pub(crate) fn sheet_entry(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let input = self.text(a.h(0)?)?;
        let currency = self.text(a.h(1)?)?;
        let (stored, format) = tilo_sheet::entry_of(&input, &currency);
        let answer = serde_json::to_string(&[stored, format]);
        self.json_result(answer.map_err(|why| why.to_string()))
    }

    /// The sheets a spreadsheet file holds, as the app keeps them.
    pub(crate) fn sheet_import(&self, file: &FileValue) -> Result<String, String> {
        let bytes = {
            let blobs = self.blobs.borrow();
            let Some(blobs) = blobs.as_ref() else {
                return Err("this host holds no content store".to_owned());
            };
            read(blobs.as_ref(), file)?
        };
        let sheets = tilo_sheet::import(&bytes, &file.name, &self.notion_words())?;
        serde_json::to_string(&tilo_sheet::sheet_rows(&sheets)).map_err(|why| why.to_string())
    }

    /// The bytes of a workbook's file, of the format its name ends in.
    pub(crate) fn sheet_export(&self, workbook: &str, name: &str) -> Result<Vec<u8>, String> {
        let book = self.book_of(workbook)?;
        let format = tilo_sheet::Format::of_name(name)
            .ok_or_else(|| format!("`{name}` is no spreadsheet file"))?;
        tilo_sheet::export(&book, format)
    }

    /// The workbook a text describes, with its cells' own date and time formats read in the
    /// language the window speaks, so every member of the workspace reads a value alike.
    fn book_of(&self, text: &str) -> Result<tilo_sheet::Book, String> {
        let mut book: tilo_sheet::Book = serde_json::from_str(text)
            .map_err(|why| format!("the workbook is not JSON: {why}"))?;
        book.language = self.words.borrow().language();
        Ok(book)
    }

    /// `answer` as the text a call answers, or the refusal it carries.
    fn json_result(&self, answer: Result<String, String>) -> HResult<Option<Wasm>> {
        self.result(answer.map(Value::text))
    }

    /// The words a Notion import says its outcome in: the app's own, in the reader's language.
    fn notion_words(&self) -> tilo_sheet::notion::Words {
        let words = self.words.borrow();
        let word = |key: &str, source: &str| words.word(key, &[], source);
        tilo_sheet::notion::Words {
            too_big: word("sheets-notion-too-big", "The export is too large to import"),
            too_many: word(
                "sheets-notion-too-many-tables",
                "The export holds more tables than one import brings",
            ),
            archive_invalid: word(
                "sheets-notion-archive-invalid",
                "The export archive cannot be read",
            ),
            unreadable: word("sheets-notion-unreadable", "This file is not a Notion export"),
            no_tables: word("sheets-notion-no-tables", "No table in this export to import"),
            imported_table: word("sheets-notion-imported-table", "Imported table"),
            yes: word("sheets-notion-yes", "Yes"),
            no: word("sheets-notion-no", "No"),
        }
    }

    /// Whether `imp` is one of the spreadsheet calls.
    pub(crate) fn is_sheet_import(imp: Imp) -> bool {
        matches!(imp, Imp::SheetFile | Imp::SheetOfFile)
    }
}

/// The sheet a text describes.
fn sheet_of(text: &str) -> Result<tilo_sheet::Sheet, String> {
    serde_json::from_str(text).map_err(|why| format!("the sheet is not JSON: {why}"))
}

/// The writes a record request makes on `sheet`: an `upsert-item` when it names `match`, else a
/// `set-records`.
fn records_of(
    sheet: &tilo_sheet::Sheet,
    request: &serde_json::Value,
) -> Result<tilo_sheet::Writes, String> {
    let records = request
        .get("records")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "give `records`: the rows to write, each by its column header".to_owned())?;
    let matching = request
        .get("matching")
        .or_else(|| request.get("match"))
        .and_then(serde_json::Value::as_object);
    if let Some(matching) = matching {
        let value = request
            .get("value")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| "`upsert` needs `value`: the row written by its headers".to_owned())?;
        return tilo_sheet::upsert(sheet, matching, value);
    }
    let row = match request.get("row") {
        None | Some(serde_json::Value::Null) => None,
        Some(row) => Some(
            u32::try_from(row.as_u64().ok_or_else(|| "`row` is a row number".to_owned())?)
                .map_err(|_| "`row` is a row number".to_owned())?,
        ),
    };
    let columns = request.get("columns").and_then(|columns| {
        columns.as_array().map(|columns| {
            columns
                .iter()
                .filter_map(|column| column.as_str().map(str::to_owned))
                .collect::<Vec<_>>()
        })
    });
    let key = request
        .get("key")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let request = tilo_sheet::RecordsRequest {
        records,
        row,
        columns: columns.as_deref(),
        key: key.as_deref(),
    };
    tilo_sheet::set_records(sheet, &request)
}

/// What a record request wrote, as the app reads it: each cell's new input and each format, by
/// reference. A cell the write leaves holds no entry, so the app reads a map of the cells that
/// changed.
fn writes_json(writes: &tilo_sheet::Writes) -> Result<String, serde_json::Error> {
    let held = |entries: &[(String, Option<String>)]| {
        entries
            .iter()
            .filter_map(|(reference, input)| {
                input.as_ref().map(|input| {
                    (
                        reference.clone(),
                        serde_json::Value::String(input.clone()),
                    )
                })
            })
            .collect::<serde_json::Map<_, _>>()
    };
    let mut out = serde_json::Map::new();
    out.insert("cells".to_owned(), held(&writes.cells).into());
    out.insert("formats".to_owned(), held(&writes.formats).into());
    serde_json::to_string(&serde_json::Value::Object(out))
}

/// The bytes a file holds, read whole.
fn read(blobs: &dyn BlobHost, file: &FileValue) -> Result<Vec<u8>, String> {
    if file.size > MAX_BYTES {
        return Err(format!("`{}` is too large to open here", file.name));
    }
    blobs
        .read(&file.hash)
        .ok_or_else(|| format!("the content of `{}` is not on this device", file.name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet() -> tilo_sheet::Sheet {
        let mut sheet = tilo_sheet::Sheet {
            name: "Sheet1".to_owned(),
            ..tilo_sheet::Sheet::default()
        };
        sheet.cells.insert("A1".to_owned(), "Name".to_owned());
        sheet.cells.insert("B1".to_owned(), "Done".to_owned());
        sheet
    }

    /// A write leaves no entry for a cell it leaves, so the app reads only what changed.
    #[test]
    fn a_records_write_names_only_the_cells_it_changed() {
        let writes = tilo_sheet::Writes {
            cells: vec![
                ("A2".to_owned(), Some("Ada".to_owned())),
                ("B2".to_owned(), None),
            ],
            formats: vec![
                ("A2".to_owned(), Some("text".to_owned())),
                ("B2".to_owned(), None),
            ],
        };
        let text = writes_json(&writes).expect("the writes are JSON");
        let json: serde_json::Value = serde_json::from_str(&text).expect("JSON text");
        assert_eq!(json["cells"]["A2"], "Ada");
        assert!(json["cells"].get("B2").is_none(), "a cell left holds none");
        assert_eq!(json["formats"]["A2"], "text");
    }

    /// The row an update identifies with `matching` is found, and one nothing matches is made.
    #[test]
    fn an_update_finds_its_row_by_the_values_it_names() {
        let sheet = sheet();
        let made = records_of(
            &sheet,
            &serde_json::json!({
                "records": [],
                "matching": {"Name": "Ada"},
                "value": {"Done": "yes"},
            }),
        )
        .expect("a new row under the headers");
        assert!(
            made.cells
                .iter()
                .any(|(reference, input)| reference == "A2" && input.as_deref() == Some("Ada")),
            "the row carries the value that identifies it: {:?}",
            made.cells
        );
        assert!(
            made.cells
                .iter()
                .any(|(reference, input)| reference == "B2" && input.as_deref() == Some("yes")),
            "and the cells it takes: {:?}",
            made.cells
        );
    }
}
