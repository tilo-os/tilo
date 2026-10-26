//! Getting a workbook in and out of the app: the chooser and its bounds, the tables a file decodes into, the import that becomes a durable action stream, and the export that writes one back.

use super::*;

impl SheetsApp {
    /// Import a spreadsheet file into new sheets: the one at `path`, or the
    /// one the person chooses. A file the workbook already holds is shown
    /// instead of imported again.
    pub(super) fn import(
        &mut self,
        format: Interchange,
        path: Option<String>,
        cx: &mut ui::framework::EffectCx<Workbook, Self>,
    ) -> ui::framework::Effected {
        let Some(path) = path else {
            self.status = format!("Choose a .{} file to import…", format.ext());
            return cx.choose_file(
                spreadsheet_import_request(format),
                move |app: &mut Self, chosen, cx| {
                    let Some(file) = chosen else {
                        app.status = "Import cancelled".into();
                        return Ok(ui::framework::Effected::done());
                    };
                    if u64::try_from(file.bytes.len()).unwrap_or(u64::MAX) > MAX_IMPORT_FILE_BYTES {
                        return app.failed(format!(
                            "{} is larger than the {} import limit",
                            file.name,
                            ui::format::memory(MAX_IMPORT_FILE_BYTES)
                        ));
                    }
                    // The same file chosen again is shown, not imported twice.
                    let source = file.identity.as_ref().map(ToString::to_string);
                    if let Some(source) = &source
                        && app.show_imported(source, &file.name, cx)
                    {
                        return Ok(ui::framework::Effected::done());
                    }
                    app.status = format!("Importing .{}…", format.ext());
                    let name = file.name;
                    let sheet = sheet_name_from_path(std::path::Path::new(&name));
                    Ok(cx.thread(
                        move |_| Self::read_tables(format, &file.bytes, &sheet),
                        move |app: &mut Self, tables, cx| {
                            app.finish_import(tables, &name, source, cx)
                        },
                    ))
                },
            );
        };
        let path = PathBuf::from(path);
        let source = import_source(&path);
        if self.show_imported(&source, &file_label(&path), cx) {
            return ui::framework::Effected::done();
        }
        self.status = format!("Importing .{}…", format.ext());
        cx.thread(
            move |_| Self::read_tables_at(format, &path).map(|tables| (tables, path)),
            move |app: &mut Self, read, cx| match read {
                Ok((tables, path)) => {
                    app.finish_import(Ok(tables), &file_label(&path), Some(source), cx)
                }
                Err(error) => app.failed(error),
            },
        )
    }

    /// Show the sheet the file `source` was already imported as, if any:
    /// whether there was one.
    pub(super) fn show_imported(
        &mut self,
        source: &str,
        label: &str,
        _cx: &ui::framework::EffectCx<Workbook, Self>,
    ) -> bool {
        let open = self
            .workbook
            .sheets
            .iter()
            .find(|(_, sheet)| {
                sheet.source.as_ref().map(|source| source.0.as_str()) == Some(source)
            })
            .map(|(id, sheet)| (id.clone(), sheet.name.0.clone()));
        let Some((sheet_id, name)) = open else {
            return false;
        };
        self.pending_reveal = Some(PendingReveal {
            sheet_id: Some(sheet_id),
            addr: (0, 0),
            status: Some(format!("{label} is already open as {name}")),
            arriving: false,
        });
        true
    }

    /// Add the sheets a file was read into as one action, and show the last
    /// of them once it is applied.
    pub(super) fn finish_import(
        &mut self,
        tables: Result<Vec<crate::xlsx::SheetTable>, String>,
        label: &str,
        source: Option<String>,
        cx: &ui::framework::EffectCx<Workbook, Self>,
    ) -> ui::framework::EffectAnswer {
        let built = tables.and_then(|tables| self.import_action(tables, source, None));
        let (invoke, total) = match built {
            Ok(built) => built,
            Err(error) => return self.failed(error),
        };
        cx.submit(invoke);
        self.status = format!("Imported {total} cells from {label}");
        self.pending_reveal = Some(PendingReveal {
            sheet_id: None,
            addr: (0, 0),
            status: Some(self.status.clone()),
            arriving: true,
        });
        Ok(ui::framework::Effected::done())
    }

    /// Write the workbook as a spreadsheet file (a CSV holds the active sheet
    /// only): to `path`, or where the person chooses. Encoding runs off the UI
    /// thread; the file is written only when the job was not cancelled.
    pub(super) fn export(
        &mut self,
        format: Interchange,
        path: Option<String>,
        cx: &mut ui::framework::EffectCx<Workbook, Self>,
    ) -> ui::framework::EffectAnswer {
        let active = self.active_sheet_id();
        let sheets: Vec<crate::workbook::SheetSnapshot> = match format {
            Interchange::Csv => active
                .as_ref()
                .and_then(|id| self.workbook.snapshot_of(id))
                .into_iter()
                .collect(),
            Interchange::Xlsx | Interchange::Ods => self.workbook_snapshots(),
        };
        let Some(name) = active
            .as_ref()
            .and_then(|id| self.workbook.sheet(id))
            .map(|sheet| sheet.name.0.clone())
            .or_else(|| sheets.first().map(|sheet| sheet.name.clone()))
        else {
            return self.failed("there is no sheet to export".into());
        };
        let _ = cx;
        self.status = format!("Exporting .{}…", format.ext());
        let encode = move |_| Self::encode_tables(format, &sheets);
        let Some(path) = path else {
            let suggested = format!("{name}.{}", format.ext());
            return Ok(cx.thread(encode, move |app: &mut Self, bytes, cx| {
                let bytes = match bytes {
                    Ok(bytes) => bytes,
                    Err(error) => return app.failed(error),
                };
                app.status = "Choose where to save…".into();
                Ok(cx.save_file(
                    spreadsheet_export_request(format, &suggested),
                    bytes,
                    move |app: &mut Self, saved, _| {
                        app.status = if saved {
                            format!("Exported .{}", format.ext())
                        } else {
                            "Export cancelled".into()
                        };
                        Ok(ui::framework::Effected::done())
                    },
                ))
            }));
        };
        let path = PathBuf::from(path);
        Ok(cx.thread(encode, move |app: &mut Self, bytes, _| {
            let written = bytes.and_then(|bytes| {
                crate::xlsx::write_file(&path, &bytes).map_err(|error| error.to_string())
            });
            match written {
                Ok(()) => {
                    app.status = format!("Exported .{} to {}", format.ext(), file_label(&path));
                    Ok(ui::framework::Effected::done())
                }
                Err(error) => app.failed(error),
            }
        }))
    }

    /// Report a failed import or export, and fail its effect.
    pub(super) fn failed(&mut self, error: String) -> ui::framework::EffectAnswer {
        self.status = format!("Spreadsheet operation failed: {error}");
        self.pending.redraw = true;
        Err(error)
    }

    /// The one verb that adds a parsed set of tables as new sheets, running
    /// smart inference where a source cell carried no explicit format, with
    /// how many cells it writes. Into a workbook that is still blank, the
    /// file replaces its empty sheet. The verb checks every tab and cell
    /// before anything is written, so collaborators never observe a
    /// half-import; history and undo see one operation.
    pub(super) fn import_action(
        &self,
        tables: Vec<crate::xlsx::SheetTable>,
        source: Option<String>,
        title: Option<String>,
    ) -> Result<(Invoke<Workbook>, usize), String> {
        use crate::resource::{CellWrite, ImportedSheet};
        let cur = self.default_currency();
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
        let values = formula::recalculate_snapshot(snap);
        let mut fmt = snap.fmt.clone();
        for (addr, raw) in &snap.raw {
            if !snap.fmt.contains_key(addr)
                && let Some(body) = raw.strip_prefix('=')
                && let Some(inferred) = formula::infer_result_format(body, &snap.fmt)
            {
                fmt.insert(*addr, inferred);
            }
        }
        crate::xlsx::SheetTable {
            hidden: snap.hidden,
            ..crate::xlsx::SheetTable::from_cells(&snap.name, &snap.raw, &fmt, &values, &snap.bold)
        }
    }

    /// Add `tables` as new sheets now, as an import does, and show the last.
    #[cfg(test)]
    pub(super) fn import_tables(
        &mut self,
        doc: &mut Edits<'_>,
        tables: Vec<crate::xlsx::SheetTable>,
    ) -> Result<usize, String> {
        let (invoke, total) = self.import_action(tables, None, None)?;
        if !self.submit(doc, invoke) {
            return Err(self.status.clone());
        }
        self.active_sheet = self.sheet_ids.len().saturating_sub(1);
        self.reload();
        Ok(total)
    }
}
