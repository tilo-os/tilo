//! Spreadsheet interchange: the [`SheetTable`] Sheets imports and exports, and
//! Excel `.xlsx` import and export.
//!
//! Both package formats go through tilo-doc's codecs ([`tilo_doc_codecs::XlsxCodec`] here,
//! [`tilo_doc_codecs::OdsCodec`] in [`crate::ods`]): an untrusted file is read under the
//! shared zip-bomb and DTD/XXE bounds into plain cells, never through a
//! dense grid, and only then materialised — within
//! [`MAX_MATERIALIZED_SLOTS`] — as a table. The exchange unit is a dense grid
//! of cells, each carrying the value the user sees plus enough format
//! information to reproduce number/currency/percent/date styling on the far
//! side. A cell that holds a **formula** carries the formula text too, so
//! `=SUM(A1:A3)` round-trips through the OOXML `<f>` element (and the ODF
//! `table:formula` attribute on the `.ods` path) instead of collapsing to its
//! last computed value — a re-import restores the live formula.

use std::collections::BTreeMap;
use std::path::Path;

use tilo_doc::sheet::a1::{make_ref, parse_ref};
use tilo_doc::{CellInput, CellType, CellView, SheetView, WorkbookSnapshot};
use tilo_doc_codecs::xlsx::{CellStyle, cell_styles, date_style_refs, number_format_codes};
use tilo_doc_codecs::{ParsedSheet, XlsxCodec};

use crate::model::{CellFormat, CellValue, format_number_general};
use tilo_locale::DateStyle;

/// The most grid slots (rows plus the cells of every row, up to its last
/// populated column) one imported tab may materialise into a [`SheetTable`].
/// A package can name a cell anywhere in the 16384 × 1048576 grid in a few
/// bytes; this keeps a sparse, far-flung sheet from turning into gigabytes of
/// empty slots. CSV import applies the same bound.
pub const MAX_MATERIALIZED_SLOTS: usize = 1_000_000;

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
    /// Bold text. Export writes it to `.xlsx`; imports leave it off.
    pub bold: bool,
}

impl ImportedCell {
    /// A plain value cell (no formula).
    fn value(raw: String, format: CellFormat) -> Self {
        Self {
            raw,
            format,
            formula: None,
            bold: false,
        }
    }
}

/// A single sheet as a row-major grid of optional cells. `None` is a blank.
#[derive(Clone, Debug, Default)]
pub struct SheetTable {
    pub name: String,
    /// Whether the file keeps the sheet hidden.
    pub hidden: bool,
    /// `rows[r][c]` — rows may be ragged.
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
        bold: &std::collections::HashSet<crate::model::Addr>,
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
                        CellValue::Number(n) => format_number_general(*n),
                        _ if is_formula => v.to_plain_string(),
                        _ => raw_s.clone(),
                    };
                    ImportedCell {
                        raw: stored_raw,
                        format: f,
                        formula: is_formula.then(|| raw_s.clone()),
                        bold: bold.contains(&addr),
                    }
                });
                row.push(cell);
            }
            rows.push(row);
        }
        Self {
            name: name.to_string(),
            hidden: false,
            rows,
        }
    }
}

/// Read every sheet of an `.xlsx` workbook into tables.
///
/// # Errors
///
/// Returns an error if the file can't be read, isn't a valid `.xlsx` package
/// (including one over the zip-bomb bounds or carrying a DTD), or a tab
/// spans more than [`MAX_MATERIALIZED_SLOTS`].
pub fn read_xlsx(path: &Path) -> anyhow::Result<Vec<SheetTable>> {
    parse_xlsx(&read_file(path)?)
}

/// Read every sheet of the `.xlsx` workbook `bytes` into tables.
pub fn parse_xlsx(bytes: &[u8]) -> anyhow::Result<Vec<SheetTable>> {
    let parsed = XlsxCodec::parse(bytes)?;
    // Excel stores dates, money and percentages as plain numbers; a cell's
    // style is the only sign of what it shows.
    let formats = parsed
        .preserved
        .get("xl/styles.xml")
        .map(|styles| style_formats(styles))
        .unwrap_or_default();
    tables_from_parsed(parsed.sheets, &formats)
}

/// What each cell style of a `styles.xml` shows a number as, by style index:
/// a date, a percentage, an amount of money or a grouped number.
fn style_formats(styles: &[u8]) -> BTreeMap<String, CellFormat> {
    let dates = date_style_refs(styles);
    let mut formats: BTreeMap<String, CellFormat> = number_format_codes(styles)
        .into_iter()
        .filter_map(|(style, code)| Some((style, format_of_code(&code)?)))
        .collect();
    for style in dates {
        formats.insert(style, CellFormat::Date(DateStyle::Iso));
    }
    formats
}

/// The Sheets format an Excel number-format code shows: a percentage, money
/// by its symbol, a number with digit grouping or decimals; `None` for the
/// general format and codes Sheets has no form for.
fn format_of_code(code: &str) -> Option<CellFormat> {
    use tilo_locale::Currency;
    // Only the first (positive) section counts.
    let first = code.split(';').next().unwrap_or(code);
    let symbols = [
        ("$", Currency::USD),
        ("\u{20AC}", Currency::EUR),
        ("\u{00A3}", Currency::GBP),
        ("\u{00A5}", Currency::JPY),
        ("\u{20B9}", Currency::INR),
        ("CHF", Currency::CHF),
        ("EUR", Currency::EUR),
        ("GBP", Currency::GBP),
    ];
    if first.contains('%') {
        return Some(CellFormat::Percent);
    }
    if let Some((_, currency)) = symbols.iter().find(|(symbol, _)| first.contains(symbol)) {
        return Some(CellFormat::Currency(*currency));
    }
    (first.contains('#') || first.contains("0.0") || first.contains(",0"))
        .then_some(CellFormat::Number)
}

/// Materialise parsed tabs as tables, turning each cell into what Sheets
/// stores: numbers in canonical form, in the format their cell style shows
/// (`style_formats`), text, `TRUE`/`FALSE` booleans, and formulas normalised
/// to `=`-prefixed A1 form.
pub(crate) fn tables_from_parsed(
    sheets: Vec<ParsedSheet>,
    style_formats: &BTreeMap<String, CellFormat>,
) -> anyhow::Result<Vec<SheetTable>> {
    sheets
        .into_iter()
        .map(|sheet| {
            let mut placed: BTreeMap<(u32, u32), ImportedCell> = BTreeMap::new();
            for (reference, input) in sheet.cells {
                let Some((col, row)) = parse_ref(&reference) else {
                    continue;
                };
                match imported_cell(input, style_formats) {
                    Some(cell) => placed.insert((row, col), cell),
                    None => placed.remove(&(row, col)),
                };
            }
            let height = placed.keys().map(|(row, _)| *row).max().unwrap_or(0);
            let mut widths = vec![0u32; height as usize];
            for (row, col) in placed.keys() {
                if let Some(width) = widths.get_mut(*row as usize - 1) {
                    *width = (*width).max(*col);
                }
            }
            let slots = widths
                .iter()
                .map(|w| *w as usize)
                .fold(height as usize, usize::saturating_add);
            if slots > MAX_MATERIALIZED_SLOTS {
                anyhow::bail!(
                    "tab {:?} spans {slots} cells, above the {MAX_MATERIALIZED_SLOTS} import limit",
                    sheet.name
                );
            }
            let mut rows: Vec<Vec<Option<ImportedCell>>> =
                widths.iter().map(|w| vec![None; *w as usize]).collect();
            for ((row, col), cell) in placed {
                if let Some(slot) = rows
                    .get_mut(row as usize - 1)
                    .and_then(|cells| cells.get_mut(col as usize - 1))
                {
                    *slot = Some(cell);
                }
            }
            Ok(SheetTable {
                hidden: sheet.hidden,
                name: sheet.name,
                rows,
            })
        })
        .collect()
}

/// One parsed cell as Sheets stores it, or `None` for a blank (a style-only
/// cell carries nothing Sheets keeps).
fn imported_cell(
    input: CellInput,
    style_formats: &BTreeMap<String, CellFormat>,
) -> Option<ImportedCell> {
    let styled = input
        .style
        .as_ref()
        .and_then(|style| style_formats.get(style))
        .copied();
    let formula = input
        .formula
        .as_deref()
        .filter(|f| !f.is_empty())
        .map(normalize_read_formula);
    let value = match input.cell_type {
        CellType::Number => match input.value.trim().parse::<f64>() {
            Ok(n) => Some(ImportedCell::value(
                format_number_general(n),
                styled.unwrap_or(CellFormat::General),
            )),
            Err(_) => (!input.value.is_empty())
                .then(|| ImportedCell::value(input.value, CellFormat::Text)),
        },
        CellType::Text | CellType::Error => {
            Some(ImportedCell::value(input.value, CellFormat::Text))
        }
        CellType::Bool => {
            let truthy = input.value.eq_ignore_ascii_case("true") || input.value == "1";
            Some(ImportedCell::value(
                if truthy { "TRUE" } else { "FALSE" }.into(),
                CellFormat::Boolean,
            ))
        }
        CellType::Empty => None,
    };
    match (value, formula) {
        (Some(mut cell), Some(formula)) => {
            cell.formula = Some(formula);
            Some(cell)
        }
        (None, Some(formula)) => Some(ImportedCell {
            raw: String::new(),
            format: styled.unwrap_or(CellFormat::General),
            formula: Some(formula),
            bold: false,
        }),
        (value, None) => value,
    }
}

/// Normalize a formula string as stored in the file into our A1 form
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
    while let Some(&c) = chars.get(i) {
        if c == '[' {
            let mut inner = String::new();
            let mut j = i + 1;
            while let Some(&ch) = chars.get(j) {
                if ch == ']' {
                    break;
                }
                inner.push(ch);
                j += 1;
            }
            // A range endpoint is `[.<sheet>.]A1`: keep only the part after the
            // final `.` (dropping the current-table dot or a sheet qualifier).
            let rewritten: Vec<String> = inner
                .split(':')
                .map(|part| match part.rfind('.') {
                    Some(dot) => part.get(dot + 1..).unwrap_or(part).to_string(),
                    None => part.to_string(),
                })
                .collect();
            out.push_str(&rewritten.join(":"));
            i = j + 1; // skip the closing ']' (or run to the end if unmatched)
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

/// The value half of an exported cell: a finite number keeps its numeric
/// type, anything else is text; a formula's empty cached result stays empty.
fn cached_value(raw: &str) -> (String, CellType) {
    if raw.is_empty() {
        (String::new(), CellType::Empty)
    } else if let Some(n) = finite_number(raw) {
        (n.to_string(), CellType::Number)
    } else {
        (raw.to_string(), CellType::Text)
    }
}

fn finite_number(raw: &str) -> Option<f64> {
    raw.parse::<f64>().ok().filter(|n| n.is_finite())
}

/// How an exported cell is written, shared by both package formats: the
/// formula (in the target's own syntax, via `formula`), a number carrying
/// `number_style` for its format, a boolean, or text. Formulas and numbers
/// take the style, and so does a bold cell of any kind; a `Text`-formatted
/// number stays text.
pub(crate) fn export_cell(
    cell: &ImportedCell,
    col: u32,
    row: u32,
    number_style: Option<String>,
    formula: impl Fn(&str) -> String,
) -> CellView {
    let (value, cell_type, style, formula) = if let Some(f) = &cell.formula {
        let (value, cell_type) = cached_value(&cell.raw);
        (value, cell_type, number_style, Some(formula(f)))
    } else if let Some(n) = finite_number(&cell.raw)
        .filter(|_| number_style.is_some() || !matches!(cell.format, CellFormat::Text))
    {
        (n.to_string(), CellType::Number, number_style, None)
    } else if matches!(cell.format, CellFormat::Boolean) {
        let value = if cell.raw.eq_ignore_ascii_case("true") {
            "true"
        } else {
            "false"
        };
        (
            value.to_string(),
            CellType::Bool,
            number_style.filter(|_| cell.bold),
            None,
        )
    } else {
        (
            cell.raw.clone(),
            CellType::Text,
            number_style.filter(|_| cell.bold),
            None,
        )
    };
    CellView {
        reference: make_ref(col, row),
        col,
        row,
        value,
        cell_type,
        formula,
        style,
    }
}

/// A snapshot of `tables` for tilo-doc's exporters, each cell written by
/// [`export_cell`] with the style `style_for` picks for its format.
pub(crate) fn snapshot_of(
    tables: &[SheetTable],
    mut style_for: impl FnMut(&ImportedCell) -> Option<String>,
    formula: impl Fn(&str) -> String,
) -> WorkbookSnapshot {
    let sheets = tables
        .iter()
        .enumerate()
        .map(|(index, table)| {
            let mut cells = Vec::new();
            for (r, row) in (1u32..).zip(&table.rows) {
                for (c, cell) in (1u32..).zip(row) {
                    if let Some(cell) = cell {
                        cells.push(export_cell(cell, c, r, style_for(cell), &formula));
                    }
                }
            }
            SheetView {
                id: index.to_string(),
                name: table.name.clone(),
                cells,
                hidden: table.hidden,
            }
        })
        .collect();
    WorkbookSnapshot {
        schema: tilo_doc::sheet::schema::SHEET_SCHEMA_ID.to_string(),
        schema_version: tilo_doc::sheet::schema::SHEET_SCHEMA_VERSION,
        sheets,
        properties: BTreeMap::new(),
    }
}

/// Write one or more tables to an `.xlsx` file. Formula cells write a live
/// `<f>` (with the last computed value cached); other numbers are written as
/// numbers (with a number-format code for currency/percent/date/time) so the
/// file opens cleanly in Excel/Sheets. Tab names Excel would refuse are made
/// valid and unique.
///
/// # Errors
///
/// Returns an error if the package can't be assembled or `path` can't be
/// written.
pub fn write_xlsx(path: &Path, tables: &[SheetTable]) -> anyhow::Result<()> {
    write_file(path, &xlsx_bytes(tables)?)?;
    Ok(())
}

/// Tables as the bytes of an `.xlsx` workbook, as [`write_xlsx`] writes it.
///
/// # Errors
///
/// Returns an error if the package can't be assembled.
pub fn xlsx_bytes(tables: &[SheetTable]) -> anyhow::Result<Vec<u8>> {
    let mut styles: Vec<CellStyle<'static>> = Vec::new();
    let snap = snapshot_of(
        tables,
        |cell| {
            let style = CellStyle {
                number_format: excel_number_format(cell.format),
                bold: cell.bold,
            };
            if style.number_format.is_none() && !style.bold {
                return None;
            }
            let index = styles.iter().position(|s| *s == style).unwrap_or_else(|| {
                styles.push(style);
                styles.len() - 1
            });
            // Style 0 is the default; style `i + 1` applies `styles[i]`.
            Some((index + 1).to_string())
        },
        |formula| formula.strip_prefix('=').unwrap_or(formula).to_string(),
    );
    let mut preserved = BTreeMap::new();
    if !styles.is_empty() {
        preserved.insert("xl/styles.xml".to_string(), cell_styles(&styles));
    }
    Ok(XlsxCodec::export_snapshot(&snap, &preserved)?)
}

/// The bytes of a file a chooser named.
///
/// # Errors
///
/// The file cannot be read (on the web: it has no native path).
pub(crate) fn read_file(path: &Path) -> Result<Vec<u8>, tilo_files::FsError> {
    let (dir, name) = tilo_files::file_at(path)?;
    tilo_files::blocking::read(&dir, &name)
}

/// Save `bytes` as the file a chooser named, keeping an existing file's link
/// and permissions.
///
/// # Errors
///
/// The file cannot be written (on the web: it has no native path).
pub(crate) fn write_file(path: &Path, bytes: &[u8]) -> Result<(), tilo_files::FsError> {
    let (dir, name) = tilo_files::file_at(path)?;
    tilo_files::blocking::save_document(&dir, &name, bytes)
}

/// Map our format to an Excel number-format code string.
fn excel_number_format(fmt: CellFormat) -> Option<&'static str> {
    use tilo_locale::Currency;
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

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::*;
    use tilo_locale::Currency;

    #[test]
    fn a_hidden_sheet_stays_hidden_through_an_xlsx() {
        let table = |name: &str, hidden: bool| SheetTable {
            name: name.into(),
            hidden,
            rows: vec![vec![Some(ImportedCell::value(
                "1".into(),
                CellFormat::General,
            ))]],
        };
        let bytes = xlsx_bytes(&[table("Shown", false), table("Rates", true)]).unwrap();
        let tables = parse_xlsx(&bytes).unwrap();
        let hidden: Vec<(&str, bool)> = tables
            .iter()
            .map(|table| (table.name.as_str(), table.hidden))
            .collect();
        assert_eq!(hidden, [("Shown", false), ("Rates", true)]);
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn xlsx_round_trips_values_formats_tabs_and_formulas() {
        {
            let dir = std::env::temp_dir();
            let path = dir.join("sheets-xlsx-test.xlsx");
            let table = SheetTable {
                hidden: false,
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
            assert_eq!(back[0].rows[0][0].as_ref().unwrap().raw, "Item");
            // The currency number survived as 100.
            let price = back[0].rows[1][1].as_ref().unwrap();
            assert_eq!(price.raw.parse::<f64>().unwrap(), 100.0);
            // The percent value survived as 0.9.
            let pct = back[0].rows[2][1].as_ref().unwrap();
            assert!((pct.raw.parse::<f64>().unwrap() - 0.9).abs() < 1e-9);
            let _ = std::fs::remove_file(&path);
        }

        {
            let path = std::env::temp_dir().join("sheets-xlsx-colliding-tabs.xlsx");
            let tables = vec![
                SheetTable {
                    hidden: false,
                    name: "Roadmap/2026".into(),
                    rows: vec![],
                },
                SheetTable {
                    hidden: false,
                    name: "roadmap?2026".into(),
                    rows: vec![],
                },
                SheetTable {
                    hidden: false,
                    name: "Roadmap_2026".into(),
                    rows: vec![],
                },
            ];
            write_xlsx(&path, &tables).unwrap();
            let back = read_xlsx(&path).unwrap();
            let names: Vec<_> = back.into_iter().map(|table| table.name).collect();
            assert_eq!(
                names,
                ["Roadmap_2026", "roadmap_2026 (2)", "Roadmap_2026 (3)"]
            );
            let _ = std::fs::remove_file(path);
        }

        // Case `xlsx_round_trips_formulas`.
        // A sheet built from live cell data — including `=SUM`/`=AVERAGE`/`=IF`
        // formulas — writes to `.xlsx` and reads back with the formula **text**
        // intact (the OOXML `<f>` element), and re-evaluating the imported raws
        // reproduces the original results. This is the real export → import path:
        // `from_cells` → `write_xlsx` → `read_xlsx` → the formula engine.
        {
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

            let table = SheetTable::from_cells(
                "Calc",
                &raw,
                &fmt,
                &values,
                &std::collections::HashSet::new(),
            );
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

        {
            // XLSX bodies have no leading '=' and plain A1 refs.
            assert_eq!(normalize_read_formula("SUM(A1:A3)"), "=SUM(A1:A3)");
            assert_eq!(normalize_read_formula("of:=SUM([.A1:.A3])"), "=SUM(A1:A3)");
            assert_eq!(normalize_read_formula("of:=[.B2]+[.C2]"), "=B2+C2");
            assert_eq!(normalize_read_formula("of:=[.$A$1]"), "=$A$1");
        }
    }
}
