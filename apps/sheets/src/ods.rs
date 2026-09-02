//! `LibreOffice` Calc `.ods` import and export.
//!
//! Reading reuses [`calamine`] (it understands ODS as well as XLSX), so the
//! import path shares [`crate::xlsx::SheetTable`] — including the formula text
//! it lifts from each cell's `table:formula`. Writing is hand-rolled: an ODS
//! file is just a ZIP archive of flat XML, so we emit the three members
//! `LibreOffice` needs — the uncompressed `mimetype` marker, a
//! `META-INF/manifest.xml`, and the `content.xml` spreadsheet body — using the
//! `zip` crate already in the dependency tree. This keeps the writer
//! dependency-light while round-tripping values, basic number/currency/
//! percent/date styling (`office:value` + a small set of named styles), and
//! live formulas (`table:formula`, in ODF `of:=SUM([.A1:.A3])` form).

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;

use crate::model::{CellFormat, Currency, addr_to_a1};
use crate::xlsx::{ImportedCell, SheetTable};

/// Read every sheet of an `.ods` document into tables (via calamine).
///
/// # Errors
///
/// Returns an error if the file cannot be opened as a valid `.ods`
/// document, or if a sheet's cell or formula range cannot be read.
pub fn read_ods(path: &Path) -> anyhow::Result<Vec<SheetTable>> {
    let mut wb: calamine::Ods<_> = calamine::open_workbook(path)?;
    crate::xlsx::read_calamine(&mut wb)
}

/// Write tables to an `.ods` document.
///
/// # Errors
///
/// Returns an error if the output file cannot be created, or if writing
/// any ZIP member (`mimetype`, the manifest, or `content.xml`) fails.
pub fn write_ods(path: &Path, tables: &[SheetTable]) -> anyhow::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut zip = zip::ZipWriter::new(file);

    // 1. `mimetype` MUST be the first entry and stored uncompressed.
    let stored =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file("mimetype", stored)?;
    zip.write_all(b"application/vnd.oasis.opendocument.spreadsheet")?;

    let deflated = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    // 2. The manifest.
    zip.start_file("META-INF/manifest.xml", deflated)?;
    zip.write_all(MANIFEST.as_bytes())?;

    // 3. The spreadsheet body.
    zip.start_file("content.xml", deflated)?;
    zip.write_all(content_xml(tables).as_bytes())?;

    zip.finish()?;
    Ok(())
}

const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0" manifest:version="1.2">
 <manifest:file-entry manifest:full-path="/" manifest:version="1.2" manifest:media-type="application/vnd.oasis.opendocument.spreadsheet"/>
 <manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/>
</manifest:manifest>"#;

/// Build the `content.xml` body for the given tables.
fn content_xml(tables: &[SheetTable]) -> String {
    let mut s = String::new();
    s.push_str(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    s.push('\n');
    s.push_str(
        r#"<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:number="urn:oasis:names:tc:opendocument:xmlns:datastyle:1.0" office:version="1.2">"#,
    );
    s.push_str("<office:automatic-styles>");
    s.push_str(NUMBER_STYLES);
    s.push_str("</office:automatic-styles>");
    s.push_str("<office:body><office:spreadsheet>");
    for table in tables {
        let _ = write!(
            s,
            r#"<table:table table:name="{}">"#,
            xml_escape_attr(&table.name)
        );
        let cols = table.rows.iter().map(std::vec::Vec::len).max().unwrap_or(0);
        let _ = write!(
            s,
            r#"<table:table-column table:number-columns-repeated="{}"/>"#,
            cols.max(1)
        );
        for row in &table.rows {
            s.push_str("<table:table-row>");
            for cell in row {
                s.push_str(&cell_xml(cell.as_ref()));
            }
            s.push_str("</table:table-row>");
        }
        s.push_str("</table:table>");
    }
    s.push_str("</office:spreadsheet></office:body></office:document-content>");
    s
}

/// A handful of named data styles for the formatted numeric kinds.
const NUMBER_STYLES: &str = r#"<number:currency-style style:name="ceUSD"><number:currency-symbol>$</number:currency-symbol><number:number number:decimal-places="2" number:min-integer-digits="1" number:grouping="true"/></number:currency-style><number:currency-style style:name="ceEUR"><number:number number:decimal-places="2" number:min-integer-digits="1" number:grouping="true"/><number:text> </number:text><number:currency-symbol>€</number:currency-symbol></number:currency-style><number:currency-style style:name="ceGBP"><number:currency-symbol>£</number:currency-symbol><number:number number:decimal-places="2" number:min-integer-digits="1" number:grouping="true"/></number:currency-style><number:currency-style style:name="ceJPY"><number:currency-symbol>¥</number:currency-symbol><number:number number:decimal-places="0" number:min-integer-digits="1" number:grouping="true"/></number:currency-style><number:currency-style style:name="ceCNY"><number:currency-symbol>¥</number:currency-symbol><number:number number:decimal-places="2" number:min-integer-digits="1" number:grouping="true"/></number:currency-style><number:currency-style style:name="ceCHF"><number:number number:decimal-places="2" number:min-integer-digits="1" number:grouping="true"/><number:text> CHF</number:text></number:currency-style><number:currency-style style:name="ceINR"><number:currency-symbol>₹</number:currency-symbol><number:number number:decimal-places="2" number:min-integer-digits="1" number:grouping="true"/></number:currency-style><number:currency-style style:name="ceCAD"><number:currency-symbol>$</number:currency-symbol><number:number number:decimal-places="2" number:min-integer-digits="1" number:grouping="true"/></number:currency-style><number:currency-style style:name="ceAUD"><number:currency-symbol>$</number:currency-symbol><number:number number:decimal-places="2" number:min-integer-digits="1" number:grouping="true"/></number:currency-style><number:percentage-style style:name="cePct"><number:number number:decimal-places="2" number:min-integer-digits="1"/><number:text>%</number:text></number:percentage-style><number:date-style style:name="ceDate"><number:year number:style="long"/><number:text>-</number:text><number:month number:style="long"/><number:text>-</number:text><number:day number:style="long"/></number:date-style><style:style style:name="coUSD" style:family="table-cell" style:data-style-name="ceUSD"/><style:style style:name="coEUR" style:family="table-cell" style:data-style-name="ceEUR"/><style:style style:name="coGBP" style:family="table-cell" style:data-style-name="ceGBP"/><style:style style:name="coJPY" style:family="table-cell" style:data-style-name="ceJPY"/><style:style style:name="coCNY" style:family="table-cell" style:data-style-name="ceCNY"/><style:style style:name="coCHF" style:family="table-cell" style:data-style-name="ceCHF"/><style:style style:name="coINR" style:family="table-cell" style:data-style-name="ceINR"/><style:style style:name="coCAD" style:family="table-cell" style:data-style-name="ceCAD"/><style:style style:name="coAUD" style:family="table-cell" style:data-style-name="ceAUD"/><style:style style:name="coPct" style:family="table-cell" style:data-style-name="cePct"/><style:style style:name="coDate" style:family="table-cell" style:data-style-name="ceDate"/>"#;

fn currency_cell_style(currency: Currency) -> &'static str {
    match currency {
        Currency::USD => "coUSD",
        Currency::EUR => "coEUR",
        Currency::GBP => "coGBP",
        Currency::JPY => "coJPY",
        Currency::CNY => "coCNY",
        Currency::CHF => "coCHF",
        Currency::INR => "coINR",
        Currency::CAD => "coCAD",
        Currency::AUD => "coAUD",
    }
}

/// Emit one `<table:table-cell>` (or an empty one). A cell holding a formula
/// carries a `table:formula` attribute (ODF `of:=…` form) alongside its cached
/// value so `LibreOffice` shows a live formula and our importer can recover it.
fn cell_xml(cell: Option<&ImportedCell>) -> String {
    let Some(cell) = cell else {
        return "<table:table-cell/>".into();
    };
    let formula_attr = match &cell.formula {
        Some(f) => format!(
            r#" table:formula="{}""#,
            xml_escape_attr(&to_ods_formula(f))
        ),
        None => String::new(),
    };
    // Numeric cells carry office:value + a value-type so Calc treats them
    // as numbers; we also attach a cell style for currency/percent/date.
    if let Ok(n) = cell.raw.parse::<f64>() {
        let style_attr = match cell.format {
            CellFormat::Currency(currency) => {
                format!(r#" table:style-name="{}""#, currency_cell_style(currency))
            }
            CellFormat::Percent => r#" table:style-name="coPct""#.to_string(),
            CellFormat::Date(_) => r#" table:style-name="coDate""#.to_string(),
            _ => String::new(),
        };
        let value_type = match cell.format {
            CellFormat::Currency(_) => "currency",
            CellFormat::Percent => "percentage",
            _ => "float",
        };
        if !matches!(cell.format, CellFormat::Text) {
            let display =
                crate::model::format_value(&crate::model::CellValue::Number(n), cell.format);
            return format!(
                r#"<table:table-cell{style}{formula} office:value-type="{vt}" office:value="{val}"><text:p>{disp}</text:p></table:table-cell>"#,
                style = style_attr,
                formula = formula_attr,
                vt = value_type,
                val = n,
                disp = xml_escape_text(&display),
            );
        }
    }
    if matches!(cell.format, CellFormat::Boolean) {
        let b = cell.raw.eq_ignore_ascii_case("true");
        return format!(
            r#"<table:table-cell{formula} office:value-type="boolean" office:boolean-value="{b}"><text:p>{disp}</text:p></table:table-cell>"#,
            formula = formula_attr,
            b = b,
            disp = if b { "TRUE" } else { "FALSE" }
        );
    }
    // String cell (also the home of formulas whose result is text).
    format!(
        r#"<table:table-cell{formula} office:value-type="string"><text:p>{disp}</text:p></table:table-cell>"#,
        formula = formula_attr,
        disp = xml_escape_text(&cell.raw)
    )
}

/// Convert one of our A1 formulas (`=SUM(A1:A3)`) into ODF form
/// (`of:=SUM([.A1:.A3])`): rewrite each cell/range reference into a bracketed,
/// current-table `[.…]` reference and prefix the `OpenFormula` namespace.
fn to_ods_formula(app_formula: &str) -> String {
    let body = app_formula.strip_prefix('=').unwrap_or(app_formula);
    format!("of:={}", rewrite_refs_to_ods(body))
}

/// Rewrite the A1 references inside a formula body into ODF `[.A1]` /
/// `[.A1:.B3]` form, leaving operators, numbers, strings and function names
/// alone. Reuses the shared reference scanner so string literals and function
/// names are correctly skipped.
fn rewrite_refs_to_ods(body: &str) -> String {
    let spans = crate::formula::references(body);
    if spans.is_empty() {
        return body.to_string();
    }
    let chars: Vec<char> = body.chars().collect();
    let mut out = String::new();
    let mut cursor = 0;
    for span in &spans {
        if let Some(slice) = chars.get(cursor..span.start) {
            out.extend(slice.iter());
        }
        if span.tl == span.br {
            let _ = write!(out, "[.{}]", addr_to_a1(span.tl.0, span.tl.1));
        } else {
            let _ = write!(
                out,
                "[.{}:.{}]",
                addr_to_a1(span.tl.0, span.tl.1),
                addr_to_a1(span.br.0, span.br.1)
            );
        }
        cursor = span.end;
    }
    if let Some(slice) = chars.get(cursor..) {
        out.extend(slice.iter());
    }
    out
}

fn xml_escape_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
fn xml_escape_attr(s: &str) -> String {
    xml_escape_text(s).replace('"', "&quot;")
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::*;
    use crate::model::{CellValue, Currency};

    fn cell(raw: &str, format: CellFormat) -> ImportedCell {
        ImportedCell {
            raw: raw.into(),
            format,
            formula: None,
        }
    }

    fn sample_table() -> SheetTable {
        SheetTable {
            name: "Budget".into(),
            rows: vec![
                vec![
                    Some(cell("Item", CellFormat::Text)),
                    Some(cell("Cost", CellFormat::Text)),
                ],
                vec![
                    Some(cell("Rent", CellFormat::Text)),
                    Some(cell("1200", CellFormat::Currency(Currency::USD))),
                ],
                vec![
                    Some(cell("Tax rate", CellFormat::Text)),
                    Some(cell("0.21", CellFormat::Percent)),
                ],
            ],
        }
    }

    #[test]
    fn ods_round_trips_through_calamine() {
        let path = std::env::temp_dir().join("sheets-ods-test.ods");
        write_ods(&path, std::slice::from_ref(&sample_table())).unwrap();
        let back = read_ods(&path).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].name, "Budget");
        assert_eq!(back[0].rows[0][0].as_ref().unwrap().raw, "Item");
        // Currency value survived as 1200.
        let rent = back[0].rows[1][1].as_ref().unwrap();
        assert_eq!(rent.raw.parse::<f64>().unwrap(), 1200.0);
        // Percent value survived as 0.21.
        let rate = back[0].rows[2][1].as_ref().unwrap();
        assert!((rate.raw.parse::<f64>().unwrap() - 0.21).abs() < 1e-9);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn ods_export_uses_the_cells_actual_currency_style() {
        for (currency, style) in [
            (Currency::USD, "coUSD"),
            (Currency::EUR, "coEUR"),
            (Currency::GBP, "coGBP"),
            (Currency::JPY, "coJPY"),
            (Currency::CNY, "coCNY"),
            (Currency::CHF, "coCHF"),
            (Currency::INR, "coINR"),
            (Currency::CAD, "coCAD"),
            (Currency::AUD, "coAUD"),
        ] {
            let xml = cell_xml(Some(&cell("12.5", CellFormat::Currency(currency))));
            assert!(
                xml.contains(&format!(r#"table:style-name="{style}""#)),
                "{currency:?} used the wrong ODF style: {xml}"
            );
        }
        assert!(NUMBER_STYLES.contains("<number:currency-symbol>€</number:currency-symbol>"));
        assert!(NUMBER_STYLES.contains("<number:currency-symbol>₹</number:currency-symbol>"));
    }

    /// Formulas written into `.ods` (as `table:formula`) come back with their
    /// text intact and recompute to the original results — the real
    /// `from_cells` → `write_ods` → `read_ods` → engine path.
    #[test]
    fn ods_round_trips_formulas() {
        use crate::model::Addr;
        use std::collections::HashMap;

        let mut raw: HashMap<Addr, String> = HashMap::new();
        raw.insert((0, 0), "5".into());
        raw.insert((0, 1), "7".into());
        raw.insert((0, 2), "=SUM(A1:A2)".into()); // 12
        raw.insert((0, 3), "=A3*2".into()); // 24
        let fmt: HashMap<Addr, CellFormat> = HashMap::new();
        let values = crate::formula::recalculate(&raw);
        let table = SheetTable::from_cells("Calc", &raw, &fmt, &values);

        let path = std::env::temp_dir().join("sheets-ods-formula-rt.ods");
        write_ods(&path, std::slice::from_ref(&table)).unwrap();
        let back = read_ods(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        // Formula text (normalized back to A1) survived the ODF round-trip.
        assert_eq!(
            back[0].rows[2][0].as_ref().unwrap().formula.as_deref(),
            Some("=SUM(A1:A2)")
        );
        assert_eq!(
            back[0].rows[3][0].as_ref().unwrap().formula.as_deref(),
            Some("=A3*2")
        );

        // Re-evaluating the imported formulas reproduces the results.
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
        assert_eq!(recomputed.get(&(0, 2)), Some(&CellValue::Number(12.0)));
        assert_eq!(recomputed.get(&(0, 3)), Some(&CellValue::Number(24.0)));
    }

    #[test]
    fn ods_formula_syntax() {
        // A1 refs become bracketed current-table refs under the of: namespace.
        assert_eq!(to_ods_formula("=SUM(A1:A3)"), "of:=SUM([.A1:.A3])");
        assert_eq!(to_ods_formula("=B2+C2"), "of:=[.B2]+[.C2]");
        // Function names and numeric literals are left alone.
        assert_eq!(to_ods_formula("=A1*2"), "of:=[.A1]*2");
    }

    #[test]
    fn content_xml_is_well_formed_enough() {
        let xml = content_xml(std::slice::from_ref(&sample_table()));
        assert!(xml.contains("office:spreadsheet"));
        assert!(xml.contains(r#"table:name="Budget""#));
        // The currency cell carries a numeric office:value.
        assert!(xml.contains(r#"office:value="1200""#));
    }

    #[test]
    fn content_xml_emits_table_formula() {
        let table = SheetTable {
            name: "S".into(),
            rows: vec![vec![Some(ImportedCell {
                raw: "12".into(),
                format: CellFormat::General,
                formula: Some("=SUM(A1:A2)".into()),
            })]],
        };
        let xml = content_xml(std::slice::from_ref(&table));
        assert!(
            xml.contains(r#"table:formula="of:=SUM([.A1:.A2])""#),
            "missing formula attr in: {xml}"
        );
        // The cached value is still present so a non-recalc viewer shows it.
        assert!(xml.contains(r#"office:value="12""#));
    }

    #[test]
    fn xml_escaping() {
        assert_eq!(xml_escape_text("a<b>&c"), "a&lt;b&gt;&amp;c");
        assert_eq!(xml_escape_attr("\"q\""), "&quot;q&quot;");
    }
}
