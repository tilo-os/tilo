//! Spreadsheets: a workbook file read as the sheets it holds, a workbook written as a file, and
//! the values a workbook's cells compute. The engine is `tilo-sheet`, which draws nothing.

use crate::abi::Imp;
use crate::blob::{BlobHost, FileValue};
use crate::core::{Args, Core, HResult, trap};
use crate::engine::Wasm;
use crate::value::Value;

/// The most a spreadsheet file may hold: an import reads it whole.
const MAX_BYTES: i64 = 64 * 1024 * 1024;

impl Core {
    /// The workbook a text describes, evaluated: each of its sheets with the extent a window shows
    /// and what each cell shows.
    pub(crate) fn sheet_values(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let text = self.text(a.h(0)?)?;
        self.result(self.values_of(&text).map(Value::text))
    }

    fn values_of(&self, text: &str) -> Result<String, String> {
        let mut book: tilo_sheet::Book = serde_json::from_str(text)
            .map_err(|why| format!("the workbook is not JSON: {why}"))?;
        // A cell's own date and time formats are the workbook's, so every member reads them
        // alike: they read in the language the window speaks.
        book.language = self.words.borrow().language();
        let shown = tilo_sheet::values(&book);
        serde_json::to_string(&shown).map_err(|why| why.to_string())
    }

    /// The sheets a spreadsheet file holds, as the text `file.sheets()` answers.
    pub(crate) fn sheet_import(&self, file: &FileValue) -> Result<String, String> {
        let blobs = self.blobs.borrow();
        let Some(blobs) = blobs.as_ref() else {
            return Err("this host holds no content store".to_owned());
        };
        let bytes = read(blobs.as_ref(), file)?;
        let sheets = tilo_sheet::import(&bytes, &file.name, &self.notion_words())?;
        serde_json::to_string(&sheets).map_err(|why| why.to_string())
    }

    /// The bytes of a workbook's file, of the format its name ends in.
    pub(crate) fn sheet_export(&self, workbook: &str, name: &str) -> Result<Vec<u8>, String> {
        let mut book: tilo_sheet::Book = serde_json::from_str(workbook)
            .map_err(|why| format!("the workbook is not JSON: {why}"))?;
        book.language = self.words.borrow().language();
        let format = tilo_sheet::Format::of_name(name)
            .ok_or_else(|| format!("`{name}` is no spreadsheet file"))?;
        tilo_sheet::export(&book, format)
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

    /// Whether `imp` is one of the spreadsheet imports.
    pub(crate) fn is_sheet_import(imp: Imp) -> bool {
        matches!(imp, Imp::SheetFile | Imp::SheetOfFile)
    }
}

/// The bytes a file holds, read whole.
fn read(blobs: &dyn BlobHost, file: &FileValue) -> Result<Vec<u8>, String> {
    if file.size > MAX_BYTES {
        return Err(format!(
            "`{}` is too large to open here",
            file.name
        ));
    }
    blobs
        .read(&file.hash)
        .ok_or_else(|| format!("the content of `{}` is not on this device", file.name))
}

/// The spreadsheet failure that never happens: a trap for an import this module does not own.
pub(crate) fn not_a_sheet() -> HResult<Option<Wasm>> {
    trap("not a spreadsheet import")
}
