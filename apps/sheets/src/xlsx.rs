//! Excel `.xlsx` import and export.
//!
//! Reading uses [`calamine`], the standard pure-Rust reader; writing uses
//! [`rust_xlsxwriter`]. The exchange unit is the [`SheetTable`] — a dense
//! grid of cells, each carrying the value the user sees plus enough format
//! information to reproduce basic number/currency/percent/date styling on
//! the far side. Formulas are exported as their **computed values** (the
//! file stays useful in Excel without a re-evaluation pass); raw formula
//! text is preserved on the CSV path where round-tripping matters most.

use std::path::Path;

use calamine::{Data, Reader};
use rust_xlsxwriter::{Format, Workbook};

use crate::model::{format_value, CellFormat, CellValue};

/// One exported/imported cell: the canonical raw to store (numbers as plain
/// numerics, text as-is) and the format to render it with.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportedCell {
    pub raw: String,
    pub format: CellFormat,
}

/// A single sheet as a row-major grid of optional cells. `None` is a blank.
#[derive(Clone, Debug, Default)]
pub struct SheetTable {
    pub name: String,
    /// `rows[r][c]` — ragged rows are tolerated on read.
    pub rows: Vec<Vec<Option<ImportedCell>>>,
}

impl SheetTable {
    /// Build an export table from the live cell data: raw inputs, formats,
    /// and computed values. Cells render through their format so the file
    /// shows what the user saw.
    pub fn from_cells(
        name: &str,
        raw: &std::collections::HashMap<crate::model::Addr, String>,
        fmt: &std::collections::HashMap<crate::model::Addr, CellFormat>,
        values: &std::collections::HashMap<crate::model::Addr, CellValue>,
    ) -> Self {
        let max_c = raw.keys().map(|a| a.0).max().unwrap_or(0);
        let max_r = raw.keys().map(|a| a.1).max().unwrap_or(0);
        let mut rows = Vec::new();
        for r in 0..=max_r {
            let mut row = Vec::new();
            for c in 0..=max_c {
                let addr = (c, r);
                let cell = raw.get(&addr).map(|raw_s| {
                    let f = fmt.get(&addr).copied().unwrap_or_default();
                    // Persist the *value* rendering, but keep numeric raw for
                    // numeric cells so a re-import re-infers cleanly.
                    let v = values.get(&addr).cloned().unwrap_or(CellValue::Empty);
                    let stored_raw = match &v {
                        CellValue::Number(n) => crate::model::format_number_general(*n),
                        _ => raw_s.clone(),
                    };
                    ImportedCell {
                        raw: stored_raw,
                        format: f,
                    }
                });
                row.push(cell);
            }
            rows.push(row);
        }
        Self {
            name: name.to_string(),
            rows,
        }
    }
}

/// Read every sheet of an `.xlsx` workbook into tables.
pub fn read_xlsx(path: &Path) -> anyhow::Result<Vec<SheetTable>> {
    let mut wb: calamine::Xlsx<_> = calamine::open_workbook(path)?;
    read_calamine(&mut wb)
}

/// Shared reader over any calamine workbook (xlsx or ods).
pub(crate) fn read_calamine<R: Reader<std::io::BufReader<std::fs::File>>>(
    wb: &mut R,
) -> anyhow::Result<Vec<SheetTable>>
where
    R::Error: std::error::Error + Send + Sync + 'static,
{
    let mut tables = Vec::new();
    let names = wb.sheet_names().to_vec();
    for name in names {
        let range = wb.worksheet_range(&name)?;
        let mut rows = Vec::new();
        for row in range.rows() {
            let mut out_row = Vec::new();
            for data in row {
                out_row.push(cell_from_data(data));
            }
            rows.push(out_row);
        }
        tables.push(SheetTable { name, rows });
    }
    Ok(tables)
}

/// Map a calamine cell to our import cell, inferring a sensible format.
fn cell_from_data(data: &Data) -> Option<ImportedCell> {
    match data {
        Data::Empty => None,
        Data::Int(i) => Some(ImportedCell {
            raw: i.to_string(),
            format: CellFormat::General,
        }),
        Data::Float(f) => Some(ImportedCell {
            raw: crate::model::format_number_general(*f),
            format: CellFormat::General,
        }),
        Data::String(s) => Some(ImportedCell {
            raw: s.clone(),
            format: CellFormat::Text,
        }),
        Data::Bool(b) => Some(ImportedCell {
            raw: if *b { "TRUE".into() } else { "FALSE".into() },
            format: CellFormat::Boolean,
        }),
        Data::DateTime(dt) => Some(ImportedCell {
            // calamine's serial matches the Excel epoch we use.
            raw: crate::model::format_number_general(dt.as_f64()),
            format: CellFormat::Date(crate::model::DateStyle::Iso),
        }),
        Data::DateTimeIso(s) | Data::DurationIso(s) => Some(ImportedCell {
            raw: s.clone(),
            format: CellFormat::Text,
        }),
        Data::Error(e) => Some(ImportedCell {
            raw: format!("#{e:?}"),
            format: CellFormat::Text,
        }),
    }
}

/// Write one or more tables to an `.xlsx` file. Numbers are written as
/// numbers (with a number-format string for currency/percent/date) so the
/// file opens cleanly in Excel/Sheets.
pub fn write_xlsx(path: &Path, tables: &[SheetTable]) -> anyhow::Result<()> {
    let mut book = Workbook::new();
    for table in tables {
        let sheet = book.add_worksheet();
        sheet.set_name(sanitize_sheet_name(&table.name))?;
        for (r, row) in table.rows.iter().enumerate() {
            for (c, cell) in row.iter().enumerate() {
                let Some(cell) = cell else { continue };
                let (row_i, col_i) = (r as u32, c as u16);
                write_cell(sheet, row_i, col_i, cell)?;
            }
        }
    }
    book.save(path)?;
    Ok(())
}

fn write_cell(
    sheet: &mut rust_xlsxwriter::Worksheet,
    row: u32,
    col: u16,
    cell: &ImportedCell,
) -> anyhow::Result<()> {
    use rust_xlsxwriter::IntoExcelData;
    // Numeric formats write a typed number with an Excel number-format code.
    if let Ok(n) = cell.raw.parse::<f64>() {
        if let Some(code) = excel_number_format(cell.format) {
            let fmt = Format::new().set_num_format(code);
            sheet.write_with_format(row, col, n, &fmt)?;
            return Ok(());
        }
        // Plain number.
        if !matches!(cell.format, CellFormat::Text) {
            n.write(sheet, row, col)?;
            return Ok(());
        }
    }
    // Booleans.
    if matches!(cell.format, CellFormat::Boolean) {
        let b = cell.raw.eq_ignore_ascii_case("true");
        sheet.write_boolean(row, col, b)?;
        return Ok(());
    }
    // Fallback: write the rendered string.
    sheet.write_string(row, col, &cell.raw)?;
    Ok(())
}

/// Map our format to an Excel number-format code string.
fn excel_number_format(fmt: CellFormat) -> Option<&'static str> {
    use crate::model::Currency;
    Some(match fmt {
        CellFormat::Number => "#,##0.00",
        CellFormat::Percent => "0.00%",
        CellFormat::Currency(c) => match c {
            Currency::USD | Currency::CAD | Currency::AUD => "$#,##0.00",
            Currency::EUR => "#,##0.00 \u{20AC}",
            Currency::GBP => "\u{00A3}#,##0.00",
            Currency::JPY => "\u{00A5}#,##0",
            Currency::CNY => "\u{00A5}#,##0.00",
            Currency::CHF => "#,##0.00 \"CHF\"",
            Currency::INR => "\u{20B9}#,##0.00",
        },
        CellFormat::Date(_) => "yyyy-mm-dd",
        CellFormat::Time(_) => "hh:mm",
        _ => return None,
    })
}

/// Excel sheet names can't exceed 31 chars or contain `[]:*?/\`.
fn sanitize_sheet_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if "[]:*?/\\".contains(c) { '_' } else { c })
        .collect();
    let trimmed = if cleaned.is_empty() {
        "Sheet".to_string()
    } else {
        cleaned
    };
    trimmed.chars().take(31).collect()
}

/// Render a cell value through a format — used by callers building a
/// `SheetTable` from already-computed values where they want the display
/// string rather than the numeric raw (e.g. the CSV path).
pub fn display(value: &CellValue, fmt: CellFormat) -> String {
    format_value(value, fmt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Currency;

    #[test]
    fn xlsx_round_trips_values_and_basic_formats() {
        let dir = std::env::temp_dir();
        let path = dir.join("sheets-xlsx-test.xlsx");
        let table = SheetTable {
            name: "Data".into(),
            rows: vec![
                vec![
                    Some(ImportedCell { raw: "Item".into(), format: CellFormat::Text }),
                    Some(ImportedCell { raw: "Price".into(), format: CellFormat::Text }),
                ],
                vec![
                    Some(ImportedCell { raw: "Widget".into(), format: CellFormat::Text }),
                    Some(ImportedCell {
                        raw: "100".into(),
                        format: CellFormat::Currency(Currency::USD),
                    }),
                ],
                vec![
                    None,
                    Some(ImportedCell { raw: "0.9".into(), format: CellFormat::Percent }),
                ],
            ],
        };
        write_xlsx(&path, std::slice::from_ref(&table)).unwrap();
        let back = read_xlsx(&path).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].name, "Data");
        // Header text survived.
        assert_eq!(back[0].rows[0][0].as_ref().unwrap().raw, "Item");
        // The currency number survived as 100.
        let price = back[0].rows[1][1].as_ref().unwrap();
        assert_eq!(price.raw.parse::<f64>().unwrap(), 100.0);
        // The percent value survived as 0.9.
        let pct = back[0].rows[2][1].as_ref().unwrap();
        assert!((pct.raw.parse::<f64>().unwrap() - 0.9).abs() < 1e-9);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn sheet_name_sanitized() {
        assert_eq!(sanitize_sheet_name("a/b:c"), "a_b_c");
        assert_eq!(sanitize_sheet_name(""), "Sheet");
        assert_eq!(sanitize_sheet_name(&"x".repeat(40)).len(), 31);
    }
}
