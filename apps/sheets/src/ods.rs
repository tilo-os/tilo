//! `LibreOffice` Calc `.ods` import and export, through qdoc's
//! [`OdsCodec`]: reading shares the bounded, XXE-hardened package reader and
//! the [`SheetTable`] materialisation of [`crate::xlsx`] (formulas come back
//! from each cell's `table:formula`, normalised to A1). Writing emits a
//! snapshot of the tables — values, live formulas in ODF `of:=SUM([.A1:.A3])`
//! form, and a small set of named styles for currency/percent/date cells,
//! declared in `styles.xml`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use tilo_doc_codecs::OdsCodec;
use tilo_doc_codecs::ods::common_styles_part;

use crate::model::{CellFormat, addr_to_a1};
use crate::xlsx::SheetTable;
use tilo_locale::Currency;

/// Read every sheet of an `.ods` document into tables.
///
/// # Errors
///
/// Returns an error if the file can't be read, isn't a valid `.ods` package
/// (including one over the zip-bomb bounds or carrying a DTD), or a tab
/// spans more than [`crate::xlsx::MAX_MATERIALIZED_SLOTS`].
pub fn read_ods(path: &Path) -> anyhow::Result<Vec<SheetTable>> {
    parse_ods(&crate::xlsx::read_file(path)?)
}

/// Read every sheet of the `.ods` document `bytes` into tables.
///
/// # Errors
///
/// As [`read_ods`], for a package that is already read.
pub fn parse_ods(bytes: &[u8]) -> anyhow::Result<Vec<SheetTable>> {
    let parsed = OdsCodec::parse(bytes)?;
    // ODF types dates explicitly (they arrive as ISO text), so no style marks
    // a number as a date.
    crate::xlsx::tables_from_parsed(parsed.sheets, &BTreeSet::new())
}

/// Write tables to an `.ods` document.
///
/// # Errors
///
/// Returns an error if the package can't be assembled or `path` can't be
/// written.
pub fn write_ods(path: &Path, tables: &[SheetTable]) -> anyhow::Result<()> {
    crate::xlsx::write_file(path, &ods_bytes(tables)?)?;
    Ok(())
}

/// Tables as the bytes of an `.ods` document, as [`write_ods`] writes it.
///
/// # Errors
///
/// Returns an error if the package can't be assembled.
pub fn ods_bytes(tables: &[SheetTable]) -> anyhow::Result<Vec<u8>> {
    let snap = crate::xlsx::snapshot_of(tables, cell_style, to_ods_formula);
    let preserved = BTreeMap::from([("styles.xml".to_string(), common_styles_part(NUMBER_STYLES))]);
    Ok(OdsCodec::export_snapshot(&snap, &preserved)?)
}

/// The named cell style (declared in [`NUMBER_STYLES`]) for a numeric cell's
/// format, if it has one.
fn cell_style(cell: &crate::xlsx::ImportedCell) -> Option<String> {
    let name = match cell.format {
        CellFormat::Currency(currency) => currency_cell_style(currency),
        CellFormat::Percent => "coPct",
        CellFormat::Date(_) => "coDate",
        _ => return None,
    };
    Some(name.to_string())
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

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::*;
    use crate::model::CellValue;
    use crate::xlsx::ImportedCell;
    use std::io::Read;

    fn cell(raw: &str, format: CellFormat) -> ImportedCell {
        ImportedCell {
            raw: raw.into(),
            format,
            formula: None,
            bold: false,
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

    /// The members of an `.ods` written by [`write_ods`].
    fn written_part(tables: &[SheetTable], part: &str) -> String {
        let path = std::env::temp_dir().join(format!(
            "sheets-ods-part-{}-{}.ods",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        write_ods(&path, tables).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut out = String::new();
        archive
            .by_name(part)
            .unwrap()
            .read_to_string(&mut out)
            .unwrap();
        out
    }

    /// ODS values, currency styles, formulas, syntax and content XML.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn ods_round_trips_values_styles_and_formulas() {
        // Case `ods_round_trips`.
        {
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

        // Case `ods_export_uses_the_cells_actual_currency_style`.
        {
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
                let table = SheetTable {
                    name: "S".into(),
                    rows: vec![vec![Some(cell("12.5", CellFormat::Currency(currency)))]],
                };
                let xml = written_part(std::slice::from_ref(&table), "content.xml");
                assert!(
                    xml.contains(&format!(r#"table:style-name="{style}""#)),
                    "{currency:?} used the wrong ODF style: {xml}"
                );
            }
            let styles = written_part(&[sample_table()], "styles.xml");
            assert!(styles.contains("<number:currency-symbol>€</number:currency-symbol>"));
            assert!(styles.contains("<number:currency-symbol>₹</number:currency-symbol>"));
            assert!(styles.contains(r#"style:name="coPct""#));
        }

        // Case `ods_round_trips_formulas`.
        // Formulas written into `.ods` (as `table:formula`) come back with their
        // text intact and recompute to the original results — the real
        // `from_cells` → `write_ods` → `read_ods` → engine path.
        {
            use crate::model::Addr;
            use std::collections::HashMap;

            let mut raw: HashMap<Addr, String> = HashMap::new();
            raw.insert((0, 0), "5".into());
            raw.insert((0, 1), "7".into());
            raw.insert((0, 2), "=SUM(A1:A2)".into()); // 12
            raw.insert((0, 3), "=A3*2".into()); // 24
            let fmt: HashMap<Addr, CellFormat> = HashMap::new();
            let values = crate::formula::recalculate(&raw);
            let table = SheetTable::from_cells(
                "Calc",
                &raw,
                &fmt,
                &values,
                &std::collections::HashSet::new(),
            );

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

        // Case `ods_formula_syntax`.
        {
            // A1 refs become bracketed current-table refs under the of: namespace.
            assert_eq!(to_ods_formula("=SUM(A1:A3)"), "of:=SUM([.A1:.A3])");
            assert_eq!(to_ods_formula("=B2+C2"), "of:=[.B2]+[.C2]");
            // Function names and numeric literals are left alone.
            assert_eq!(to_ods_formula("=A1*2"), "of:=[.A1]*2");
        }

        // Case `content_xml_carries_values_formulas_and_escaped_names`.
        {
            let mut table = sample_table();
            table.name = "Budget <&> \"Q1\"".into();
            table.rows.push(vec![Some(ImportedCell {
                raw: "12".into(),
                format: CellFormat::General,
                formula: Some("=SUM(A1:A2)".into()),
                bold: false,
            })]);
            let xml = written_part(std::slice::from_ref(&table), "content.xml");
            assert!(xml.contains("office:spreadsheet"));
            assert!(xml.contains(r#"table:name="Budget &lt;&amp;&gt; &quot;Q1&quot;""#));
            // The currency cell carries a numeric office:value.
            assert!(xml.contains(r#"office:value="1200""#));
            assert!(
                xml.contains(r#"table:formula="of:=SUM([.A1:.A2])""#),
                "missing formula attr in: {xml}"
            );
            // The cached value is still present so a non-recalc viewer shows it.
            assert!(xml.contains(r#"office:value="12""#));
        }
    }
}
