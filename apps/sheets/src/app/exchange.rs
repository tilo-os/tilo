//! Getting a workbook in and out of the app: the bounds of a file, the tables it decodes into, the import that becomes a durable action stream, and the bytes an export writes back.

use super::{
    CellFormat, Currency, Interchange, Invoke, MAX_CSV_MATERIALIZED_SLOTS, MAX_IMPORT_CELLS,
    MAX_IMPORT_FILE_BYTES, SheetsApp, Workbook, file_label, infer_entry, sheet_name_from_path,
    verbs,
};

impl SheetsApp {
    /// The one verb that adds a parsed set of tables as new sheets, running
    /// smart inference where a source cell carried no explicit format, with
    /// how many cells it writes. Into a workbook that is still blank, the
    /// file replaces its empty sheet. The verb checks every tab and cell
    /// before anything is written, so collaborators never observe a
    /// half-import; history and undo see one operation.
    pub(super) fn import_action(
        tables: Vec<crate::xlsx::SheetTable>,
        source: Option<String>,
        title: Option<String>,
        cur: Currency,
    ) -> Result<(Invoke<Workbook>, usize), String> {
        use crate::resource::{CellWrite, ImportedSheet};
        let mut total: usize = 0;
        let mut sheets = Vec::new();
        for table in tables {
            let mut writes = Vec::new();
            for (r, row) in (0u32..).zip(&table.rows) {
                for (c, cell) in (0u32..).zip(row) {
                    let Some(cell) = cell else { continue };
                    // A formula cell restores its live formula; a value cell
                    // stores its value. The format still derives from the
                    // imported value, not the formula text.
                    let raw = cell.formula.as_deref().unwrap_or(cell.raw.as_str());
                    let fmt = if cell.format == CellFormat::General {
                        infer_entry(&cell.raw, cur).format
                    } else {
                        cell.format
                    };
                    writes.push((
                        (r, c),
                        CellWrite {
                            cell: crate::model::addr_to_a1(c, r),
                            input: Some(raw.to_string()),
                            format: (fmt != CellFormat::General).then_some(fmt),
                            bold: cell.bold.then_some(true),
                        },
                    ));
                }
            }
            total = total.saturating_add(writes.len());
            if total > MAX_IMPORT_CELLS {
                return Err(format!(
                    "the workbook contains more than {MAX_IMPORT_CELLS} populated cells"
                ));
            }
            writes.sort_by_key(|(at, _)| *at);
            let name = if table.name.trim().is_empty() {
                "Imported".to_string()
            } else {
                table.name
            };
            sheets.push(ImportedSheet {
                name,
                cells: writes.into_iter().map(|(_, write)| write).collect(),
                hidden: table.hidden,
            });
        }
        let invoke = Workbook::IMPORT_SHEETS.with(verbs::ImportSheets {
            sheets,
            source,
            title,
        });
        Ok((invoke, total))
    }

    /// Refuse a path before handing it to an interchange reader.  Archive
    /// formats can still expand internally, so the parsed-cell bound below is
    /// the authoritative limit; this inexpensive first gate avoids allocating
    /// an arbitrary local file just to discover it was never eligible.
    pub(super) fn check_import_file_size(path: &std::path::Path) -> Result<(), String> {
        let metadata = tilo_files::file_at(path)
            .and_then(|(dir, name)| tilo_files::blocking::metadata(&dir, &name))
            .map_err(|error| format!("cannot inspect {}: {error}", file_label(path)))?;
        if metadata.kind != tilo_files::Kind::File {
            return Err("the selected path is not a regular file".into());
        }
        if metadata.len > MAX_IMPORT_FILE_BYTES {
            return Err(format!(
                "{} is larger than the {} import limit",
                file_label(path),
                ui::format::memory(MAX_IMPORT_FILE_BYTES)
            ));
        }
        Ok(())
    }

    /// The tables of the spreadsheet file at `path`.
    pub(super) fn read_tables_at(
        format: Interchange,
        path: &std::path::Path,
    ) -> Result<Vec<crate::xlsx::SheetTable>, String> {
        Self::check_import_file_size(path)?;
        let bytes = crate::xlsx::read_file(path).map_err(|error| error.to_string())?;
        Self::read_tables(format, &bytes, &sheet_name_from_path(path))
    }

    /// The tables a spreadsheet file holds, one per sheet; a CSV is one,
    /// named `csv_name`.
    pub(super) fn read_tables(
        format: Interchange,
        bytes: &[u8],
        csv_name: &str,
    ) -> Result<Vec<crate::xlsx::SheetTable>, String> {
        match format {
            Interchange::Notion => Self::read_notion_tables(bytes),
            Interchange::Xlsx => crate::xlsx::parse_xlsx(bytes).map_err(|error| error.to_string()),
            Interchange::Ods => crate::ods::parse_ods(bytes).map_err(|error| error.to_string()),
            Interchange::Csv => {
                let text = std::str::from_utf8(bytes)
                    .map_err(|_| "the CSV file is not UTF-8 text".to_string())?;
                let cells = crate::csv::parse_csv(text);
                if cells.len() > MAX_IMPORT_CELLS {
                    return Err(format!("more than {MAX_IMPORT_CELLS} populated cells"));
                }
                let max_col = cells.keys().map(|addr| addr.0).max().unwrap_or(0);
                let max_row = cells.keys().map(|addr| addr.1).max().unwrap_or(0);
                if max_col > 16_383 || max_row > 1_048_575 {
                    return Err("a cell is outside the supported workbook grid".into());
                }
                let width = usize::try_from(max_col)
                    .map_err(|_| "column index is too large")?
                    .checked_add(1)
                    .ok_or_else(|| "column count overflows this platform".to_string())?;
                let height = usize::try_from(max_row)
                    .map_err(|_| "row index is too large")?
                    .checked_add(1)
                    .ok_or_else(|| "row count overflows this platform".to_string())?;
                let slots = width
                    .checked_mul(height)
                    .ok_or_else(|| "CSV dimensions overflow this platform".to_string())?;
                if slots > MAX_CSV_MATERIALIZED_SLOTS {
                    return Err(format!(
                        "CSV dimensions require {slots} cells, above the {MAX_CSV_MATERIALIZED_SLOTS} import limit"
                    ));
                }
                let mut rows = vec![vec![None; width]; height];
                for ((col, row), raw) in cells {
                    let c = usize::try_from(col).map_err(|_| "column index is too large")?;
                    let r = usize::try_from(row).map_err(|_| "row index is too large")?;
                    let Some(line) = rows.get_mut(r) else {
                        return Err("row is outside import bounds".into());
                    };
                    let Some(cell) = line.get_mut(c) else {
                        return Err("column is outside import bounds".into());
                    };
                    *cell = Some(crate::xlsx::ImportedCell {
                        raw,
                        format: CellFormat::General,
                        formula: None,
                        bold: false,
                    });
                }
                Ok(vec![crate::xlsx::SheetTable {
                    name: csv_name.to_string(),
                    hidden: false,
                    rows,
                }])
            }
        }
    }

    /// The tables a Notion export holds, each read the way a CSV file is.
    pub(super) fn read_notion_tables(bytes: &[u8]) -> Result<Vec<crate::xlsx::SheetTable>, String> {
        let mut tables = Vec::new();
        for table in crate::notion::tables(bytes)? {
            let mut read = Self::read_tables(Interchange::Csv, table.csv.as_bytes(), &table.name)?;
            tables.append(&mut read);
        }
        Ok(tables)
    }

    /// `sheets` as the bytes of a spreadsheet file; a CSV holds the first.
    pub(super) fn encode_tables(
        format: Interchange,
        sheets: &[crate::workbook::SheetSnapshot],
    ) -> Result<Vec<u8>, String> {
        let tables = sheets
            .iter()
            .map(Self::table_from_snapshot)
            .collect::<Vec<_>>();
        match format {
            Interchange::Notion => Err("a Notion export is read, not written".to_string()),
            Interchange::Xlsx => {
                crate::xlsx::xlsx_bytes(&tables).map_err(|error| error.to_string())
            }
            Interchange::Ods => crate::ods::ods_bytes(&tables).map_err(|error| error.to_string()),
            Interchange::Csv => {
                let table = tables
                    .first()
                    .ok_or_else(|| "there is no active sheet to export".to_string())?;
                let rows = table
                    .rows
                    .iter()
                    .map(|row| {
                        row.iter()
                            .map(|cell| {
                                cell.as_ref()
                                    .map_or_else(String::new, |cell| cell.raw.clone())
                            })
                            .collect()
                    })
                    .collect::<Vec<Vec<String>>>();
                Ok(crate::csv::to_csv(&rows).into_bytes())
            }
        }
    }

    pub(super) fn table_from_snapshot(
        snap: &crate::workbook::SheetSnapshot,
    ) -> crate::xlsx::SheetTable {
        let values = crate::calc::recalculate_snapshot(snap);
        let fmt = crate::calc::effective_formats(snap);
        crate::xlsx::SheetTable {
            hidden: snap.hidden,
            ..crate::xlsx::SheetTable::from_cells(&snap.name, &snap.raw, &fmt, &values, &snap.bold)
        }
    }

    /// Add `tables` as new sheets now, as an import does, and show the last.
    #[cfg(test)]
    pub(super) fn import_tables(
        &mut self,
        doc: &mut ui::widget::Edits<'_>,
        tables: Vec<crate::xlsx::SheetTable>,
    ) -> Result<usize, String> {
        let (invoke, total) = Self::import_action(tables, None, None, self.default_currency())?;
        if !self.submit(doc, invoke) {
            return Err(self.status.clone());
        }
        self.active_sheet = self.sheet_ids.len().saturating_sub(1);
        self.reload();
        Ok(total)
    }
}
