//! Excel `.xlsx` import and export.
//!
//! Reading uses [`calamine`], the standard pure-Rust reader; writing uses
//! [`rust_xlsxwriter`]. The exchange unit is the [`SheetTable`] — a dense
//! grid of cells, each carrying the value the user sees plus enough format
//! information to reproduce basic number/currency/percent/date styling on
//! the far side. A cell that holds a **formula** carries the formula text
//! too, so `=SUM(A1:A3)` round-trips through the OOXML `<f>` element (and the
//! ODF `table:formula` attribute on the `.ods` path) instead of collapsing to
//! its last computed value — a re-import restores the live formula.

use std::path::Path;

use calamine::{Data, Range, Reader};
use rust_xlsxwriter::{Format, Formula, Workbook};

use crate::model::{CellFormat, CellValue, format_value};

/// One exported/imported cell: the canonical raw to store (numbers as plain
/// numerics, text as-is), the format to render it with, and — when the cell
/// holds a formula — the formula text (with its leading `=`) so it survives a
/// round-trip through the file instead of being flattened to its result.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportedCell {
    pub raw: String,
    pub format: CellFormat,
    /// The cell's formula, `=`-prefixed (e.g. `=SUM(A1:A3)`), or `None` for a
    /// plain value. Written into / read back from the OOXML `<f>` element and
    /// the ODF `table:formula` attribute.
    pub formula: Option<String>,
}

impl ImportedCell {
    /// A plain value cell (no formula).
    fn value(raw: String, format: CellFormat) -> Self {
        Self {
            raw,
            format,
            formula: None,
        }
    }
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
    /// and computed values. A formula cell keeps its formula text (so the
    /// file stores a live `=SUM(...)`) while also carrying the last computed
    /// value as its cached result; plain cells render through their format so
    /// the file shows what the user saw.
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
                    let v = values.get(&addr).cloned().unwrap_or(CellValue::Empty);
                    let is_formula = raw_s.starts_with('=');
                    // Store the cell's *value* as its payload (numeric raw for
                    // numbers so a re-import re-infers cleanly, the plain
                    // rendering otherwise). For a formula this is the cached
                    // result; the formula itself rides in `formula`.
                    let stored_raw = match &v {
                        CellValue::Number(n) => crate::model::format_number_general(*n),
                        _ if is_formula => v.to_plain_string(),
                        _ => raw_s.clone(),
                    };
                    ImportedCell {
                        raw: stored_raw,
                        format: f,
                        formula: is_formula.then(|| raw_s.clone()),
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

/// Shared reader over any calamine workbook (xlsx or ods). Values come from
/// the worksheet range; a parallel formula range supplies each cell's live
/// formula (the OOXML `<f>` / ODF `table:formula`) so formulas survive import.
pub(crate) fn read_calamine<R: Reader<std::io::BufReader<std::fs::File>>>(
    wb: &mut R,
) -> anyhow::Result<Vec<SheetTable>>
where
    R::Error: std::error::Error + Send + Sync + 'static,
{
    let mut tables = Vec::new();
    let names = wb.sheet_names();
    for name in names {
        let range = wb.worksheet_range(&name)?;
        // Formulas are optional: a value-only workbook (or a reader hiccup)
        // just yields an empty formula range, so plain cells still import.
        let formulas: Range<String> = wb
            .worksheet_formula(&name)
            .unwrap_or_else(|_| Range::empty());
        let origin = range.start().unwrap_or((0, 0));
        let mut rows = Vec::new();
        for (r, row) in range.rows().enumerate() {
            let mut out_row = Vec::new();
            for (c, data) in row.iter().enumerate() {
                // Absolute sheet coordinates so the (independently-bounded)
                // formula range lines up with the value cell.
                let abs = (origin.0 + r as u32, origin.1 + c as u32);
                let formula = formulas
                    .get_value(abs)
                    .filter(|f| !f.is_empty())
                    .map(|f| normalize_read_formula(f));
                let mut cell = cell_from_data(data);
                if let Some(f) = formula {
                    cell = Some(match cell {
                        Some(mut ic) => {
                            ic.formula = Some(f);
                            ic
                        }
                        None => ImportedCell {
                            raw: String::new(),
                            format: CellFormat::General,
                            formula: Some(f),
                        },
                    });
                }
                out_row.push(cell);
            }
            rows.push(out_row);
        }
        tables.push(SheetTable { name, rows });
    }
    Ok(tables)
}

/// Normalize a formula string as reported by the reader into our A1 form
/// (`=SUM(A1:A3)`):
///   * XLSX `<f>` bodies arrive without the leading `=` and use A1 refs, so we
///     just prepend `=`.
///   * ODF `table:formula` arrives as `of:=SUM([.A1:.A3])`: strip the
///     namespace prefix and rewrite the bracketed `[.A1]` references to bare
///     A1 notation.
fn normalize_read_formula(reported: &str) -> String {
    let mut s = reported.trim();
    for pref in ["of:", "oooc:", "msoxl:"] {
        if let Some(rest) = s.strip_prefix(pref) {
            s = rest;
            break;
        }
    }
    let body = s.strip_prefix('=').unwrap_or(s);
    format!("={}", strip_ods_bracket_refs(body))
}

/// Rewrite ODF bracketed cell references — `[.A1]`, `[.A1:.B3]`, or a
/// cross-table `[Sheet2.A1]` — into bare A1 notation (`A1`, `A1:B3`, `A1`).
/// Input without brackets (an Excel formula) passes through unchanged.
fn strip_ods_bracket_refs(body: &str) -> String {
    let chars: Vec<char> = body.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '[' {
            let mut inner = String::new();
            let mut j = i + 1;
            while j < chars.len() && chars[j] != ']' {
                inner.push(chars[j]);
                j += 1;
            }
            // A range endpoint is `[.<sheet>.]A1`: keep only the part after the
            // final `.` (dropping the current-table dot or a sheet qualifier).
            let rewritten: Vec<String> = inner
                .split(':')
                .map(|part| match part.rfind('.') {
                    Some(dot) => part[dot + 1..].to_string(),
                    None => part.to_string(),
                })
                .collect();
            out.push_str(&rewritten.join(":"));
            i = j + 1; // skip the closing ']' (or run to the end if unmatched)
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// Map a calamine cell to our import cell, inferring a sensible format.
fn cell_from_data(data: &Data) -> Option<ImportedCell> {
    match data {
        Data::Empty => None,
        Data::Int(i) => Some(ImportedCell::value(i.to_string(), CellFormat::General)),
        Data::Float(f) => Some(ImportedCell::value(
            crate::model::format_number_general(*f),
            CellFormat::General,
        )),
        Data::String(s) => Some(ImportedCell::value(s.clone(), CellFormat::Text)),
        Data::Bool(b) => Some(ImportedCell::value(
            if *b { "TRUE".into() } else { "FALSE".into() },
            CellFormat::Boolean,
        )),
        Data::DateTime(dt) => Some(ImportedCell::value(
            // calamine's serial matches the Excel epoch we use.
            crate::model::format_number_general(dt.as_f64()),
            CellFormat::Date(crate::model::DateStyle::Iso),
        )),
        Data::DateTimeIso(s) | Data::DurationIso(s) => {
            Some(ImportedCell::value(s.clone(), CellFormat::Text))
        }
        Data::Error(e) => Some(ImportedCell::value(format!("#{e:?}"), CellFormat::Text)),
    }
}

/// Write one or more tables to an `.xlsx` file. Formula cells write a live
/// `<f>` (with the last computed value cached); other numbers are written as
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
    // Formula cells write a live `<f>` element; the last computed value is
    // cached so a non-recalculating viewer still shows a result. The number
    // format (currency/percent/date) rides along when the cell has one.
    if let Some(f) = &cell.formula {
        let mut formula = Formula::new(f.as_str());
        if !cell.raw.is_empty() {
            formula = formula.set_result(cell.raw.clone());
        }
        if let Some(code) = excel_number_format(cell.format) {
            let fmt = Format::new().set_num_format(code);
            sheet.write_formula_with_format(row, col, formula, &fmt)?;
        } else {
            sheet.write_formula(row, col, formula)?;
        }
        return Ok(());
    }
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
                    Some(ImportedCell::value("Item".into(), CellFormat::Text)),
                    Some(ImportedCell::value("Price".into(), CellFormat::Text)),
                ],
                vec![
                    Some(ImportedCell::value("Widget".into(), CellFormat::Text)),
                    Some(ImportedCell::value(
                        "100".into(),
                        CellFormat::Currency(Currency::USD),
                    )),
                ],
                vec![
                    None,
                    Some(ImportedCell::value("0.9".into(), CellFormat::Percent)),
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

    /// A sheet built from live cell data — including `=SUM`/`=AVERAGE`/`=IF`
    /// formulas — writes to `.xlsx` and reads back with the formula **text**
    /// intact (the OOXML `<f>` element), and re-evaluating the imported raws
    /// reproduces the original results. This is the real export → import path:
    /// `from_cells` → `write_xlsx` → `read_xlsx` → the formula engine.
    #[test]
    fn xlsx_round_trips_formulas() {
        use crate::model::Addr;
        use std::collections::HashMap;

        // A1:A3 = 10/20/30, A4 = SUM, A5 = AVERAGE, A6 = an IF returning text.
        // Anchored at A1 so the file's used range starts top-left (no shift).
        let mut raw: HashMap<Addr, String> = HashMap::new();
        raw.insert((0, 0), "10".into());
        raw.insert((0, 1), "20".into());
        raw.insert((0, 2), "30".into());
        raw.insert((0, 3), "=SUM(A1:A3)".into());
        raw.insert((0, 4), "=AVERAGE(A1:A3)".into());
        raw.insert((0, 5), "=IF(A4>50,\"BIG\",\"SMALL\")".into());
        let fmt: HashMap<Addr, CellFormat> = HashMap::new();
        let values = crate::formula::recalculate(&raw);
        // Sanity: the source really computed.
        assert_eq!(values.get(&(0, 3)), Some(&CellValue::Number(60.0)));

        let table = SheetTable::from_cells("Calc", &raw, &fmt, &values);
        let path = std::env::temp_dir().join("sheets-xlsx-formula-rt.xlsx");
        write_xlsx(&path, std::slice::from_ref(&table)).unwrap();
        let back = read_xlsx(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        // The formula TEXT survived the file (not just its computed value).
        let sum = back[0].rows[3][0].as_ref().unwrap();
        assert_eq!(sum.formula.as_deref(), Some("=SUM(A1:A3)"));
        let avg = back[0].rows[4][0].as_ref().unwrap();
        assert_eq!(avg.formula.as_deref(), Some("=AVERAGE(A1:A3)"));
        let iff = back[0].rows[5][0].as_ref().unwrap();
        assert_eq!(iff.formula.as_deref(), Some("=IF(A4>50,\"BIG\",\"SMALL\")"));

        // Re-evaluating the imported raws (formula where present, else value)
        // reproduces the original results.
        let mut reimported: HashMap<Addr, String> = HashMap::new();
        for (r, row) in back[0].rows.iter().enumerate() {
            for (c, cell) in row.iter().enumerate() {
                if let Some(cell) = cell {
                    let raw = cell.formula.clone().unwrap_or_else(|| cell.raw.clone());
                    if !raw.is_empty() {
                        reimported.insert((c as u32, r as u32), raw);
                    }
                }
            }
        }
        let recomputed = crate::formula::recalculate(&reimported);
        assert_eq!(recomputed.get(&(0, 3)), Some(&CellValue::Number(60.0)));
        assert_eq!(recomputed.get(&(0, 4)), Some(&CellValue::Number(20.0)));
        // IF(A4>50,…) with A4 = 60 takes the true branch.
        assert_eq!(
            recomputed.get(&(0, 5)),
            Some(&CellValue::Text("BIG".into()))
        );
    }

    #[test]
    fn read_formula_normalization() {
        // XLSX bodies have no leading '=' and plain A1 refs.
        assert_eq!(normalize_read_formula("SUM(A1:A3)"), "=SUM(A1:A3)");
        // ODF formulas carry a namespace prefix and bracketed refs.
        assert_eq!(normalize_read_formula("of:=SUM([.A1:.A3])"), "=SUM(A1:A3)");
        assert_eq!(normalize_read_formula("of:=[.B2]+[.C2]"), "=B2+C2");
        // A cross-table qualifier is dropped down to the bare cell.
        assert_eq!(normalize_read_formula("of:=[.$A$1]"), "=$A$1");
    }

    #[test]
    fn sheet_name_sanitized() {
        assert_eq!(sanitize_sheet_name("a/b:c"), "a_b_c");
        assert_eq!(sanitize_sheet_name(""), "Sheet");
        assert_eq!(sanitize_sheet_name(&"x".repeat(40)).len(), 31);
    }
}
