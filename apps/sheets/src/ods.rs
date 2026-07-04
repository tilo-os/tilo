//! LibreOffice Calc `.ods` import and export.
//!
//! Reading reuses [`calamine`] (it understands ODS as well as XLSX), so the
//! import path shares [`crate::xlsx::SheetTable`]. Writing is hand-rolled:
//! an ODS file is just a ZIP archive of flat XML, so we emit the three
//! members LibreOffice needs — the uncompressed `mimetype` marker, a
//! `META-INF/manifest.xml`, and the `content.xml` spreadsheet body — using
//! the `zip` crate already in the dependency tree. This keeps the writer
//! dependency-light while round-tripping values and basic number/currency/
//! percent/date styling via `office:value` + a small set of named styles.

use std::io::Write;
use std::path::Path;

use crate::model::CellFormat;
use crate::xlsx::{ImportedCell, SheetTable};

/// Read every sheet of an `.ods` document into tables (via calamine).
pub fn read_ods(path: &Path) -> anyhow::Result<Vec<SheetTable>> {
    let mut wb: calamine::Ods<_> = calamine::open_workbook(path)?;
    crate::xlsx::read_calamine(&mut wb)
}

/// Write tables to an `.ods` document.
pub fn write_ods(path: &Path, tables: &[SheetTable]) -> anyhow::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut zip = zip::ZipWriter::new(file);

    // 1. `mimetype` MUST be the first entry and stored uncompressed.
    let stored = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored);
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
        s.push_str(&format!(
            r#"<table:table table:name="{}">"#,
            xml_escape_attr(&table.name)
        ));
        let cols = table.rows.iter().map(|r| r.len()).max().unwrap_or(0);
        s.push_str(&format!(
            r#"<table:table-column table:number-columns-repeated="{}"/>"#,
            cols.max(1)
        ));
        for row in &table.rows {
            s.push_str("<table:table-row>");
            for cell in row {
                s.push_str(&cell_xml(cell));
            }
            s.push_str("</table:table-row>");
        }
        s.push_str("</table:table>");
    }
    s.push_str("</office:spreadsheet></office:body></office:document-content>");
    s
}

/// A handful of named data styles for the formatted numeric kinds.
const NUMBER_STYLES: &str = r#"<number:currency-style style:name="ceUSD"><number:currency-symbol>$</number:currency-symbol><number:number number:decimal-places="2" number:min-integer-digits="1" number:grouping="true"/></number:currency-style><number:percentage-style style:name="cePct"><number:number number:decimal-places="2" number:min-integer-digits="1"/><number:text>%</number:text></number:percentage-style><number:date-style style:name="ceDate"><number:year number:style="long"/><number:text>-</number:text><number:month number:style="long"/><number:text>-</number:text><number:day number:style="long"/></number:date-style><style:style style:name="coCur" style:family="table-cell" style:data-style-name="ceUSD"/><style:style style:name="coPct" style:family="table-cell" style:data-style-name="cePct"/><style:style style:name="coDate" style:family="table-cell" style:data-style-name="ceDate"/>"#;

/// Emit one `<table:table-cell>` (or an empty one).
fn cell_xml(cell: &Option<ImportedCell>) -> String {
    let Some(cell) = cell else {
        return "<table:table-cell/>".into();
    };
    // Numeric cells carry office:value + a value-type so Calc treats them
    // as numbers; we also attach a cell style for currency/percent/date.
    if let Ok(n) = cell.raw.parse::<f64>() {
        let style_attr = match cell.format {
            CellFormat::Currency(_) => r#" table:style-name="coCur""#,
            CellFormat::Percent => r#" table:style-name="coPct""#,
            CellFormat::Date(_) => r#" table:style-name="coDate""#,
            _ => "",
        };
        let value_type = match cell.format {
            CellFormat::Currency(_) => "currency",
            CellFormat::Percent => "percentage",
            _ => "float",
        };
        if !matches!(cell.format, CellFormat::Text) {
            let display = crate::model::format_value(&crate::model::CellValue::Number(n), cell.format);
            return format!(
                r#"<table:table-cell{style} office:value-type="{vt}" office:value="{val}"><text:p>{disp}</text:p></table:table-cell>"#,
                style = style_attr,
                vt = value_type,
                val = n,
                disp = xml_escape_text(&display),
            );
        }
    }
    if matches!(cell.format, CellFormat::Boolean) {
        let b = cell.raw.eq_ignore_ascii_case("true");
        return format!(
            r#"<table:table-cell office:value-type="boolean" office:boolean-value="{b}"><text:p>{}</text:p></table:table-cell>"#,
            if b { "TRUE" } else { "FALSE" }
        );
    }
    // String cell.
    format!(
        r#"<table:table-cell office:value-type="string"><text:p>{}</text:p></table:table-cell>"#,
        xml_escape_text(&cell.raw)
    )
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
mod tests {
    use super::*;
    use crate::model::Currency;

    fn sample_table() -> SheetTable {
        SheetTable {
            name: "Budget".into(),
            rows: vec![
                vec![
                    Some(ImportedCell { raw: "Item".into(), format: CellFormat::Text }),
                    Some(ImportedCell { raw: "Cost".into(), format: CellFormat::Text }),
                ],
                vec![
                    Some(ImportedCell { raw: "Rent".into(), format: CellFormat::Text }),
                    Some(ImportedCell {
                        raw: "1200".into(),
                        format: CellFormat::Currency(Currency::USD),
                    }),
                ],
                vec![
                    Some(ImportedCell { raw: "Tax rate".into(), format: CellFormat::Text }),
                    Some(ImportedCell { raw: "0.21".into(), format: CellFormat::Percent }),
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
    fn content_xml_is_well_formed_enough() {
        let xml = content_xml(std::slice::from_ref(&sample_table()));
        assert!(xml.contains("office:spreadsheet"));
        assert!(xml.contains(r#"table:name="Budget""#));
        // The currency cell carries a numeric office:value.
        assert!(xml.contains(r#"office:value="1200""#));
    }

    #[test]
    fn xml_escaping() {
        assert_eq!(xml_escape_text("a<b>&c"), "a&lt;b&gt;&amp;c");
        assert_eq!(xml_escape_attr("\"q\""), "&quot;q&quot;");
    }
}
