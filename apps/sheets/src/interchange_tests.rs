//! What Sheets reads from real-world `.xlsx`/`.ods` files and round-trips
//! through its own exports, pinned on packages shaped like the ones Excel and
//! `LibreOffice` Calc write: shared strings, shared formulas, date number
//! formats, booleans, compressed repeated cells and rows, several tabs.

use std::io::{Cursor, Write};
use std::path::PathBuf;

use crate::model::{CellFormat, a1_to_addr};
use crate::xlsx::{ImportedCell, SheetTable};
use tilo_locale::{Currency, DateStyle};

fn zip_of(entries: &[(&str, &str)]) -> Vec<u8> {
    let mut zw = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let stored =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let deflated = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, data) in entries {
        let options = if *name == "mimetype" {
            stored
        } else {
            deflated
        };
        zw.start_file(*name, options).unwrap();
        zw.write_all(data.as_bytes()).unwrap();
    }
    zw.finish().unwrap().into_inner()
}

/// A temporary file that removes itself.
struct TempFile(PathBuf);

impl TempFile {
    fn with(name: &str, bytes: &[u8]) -> Self {
        let path = std::env::temp_dir().join(format!(
            "sheets-interchange-{}-{}-{name}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, bytes).unwrap();
        Self(path)
    }

    fn empty(name: &str) -> Self {
        Self::with(name, b"")
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn at<'a>(table: &'a SheetTable, a1: &str) -> Option<&'a ImportedCell> {
    let (col, row) = a1_to_addr(a1).unwrap();
    table.rows.get(row as usize)?.get(col as usize)?.as_ref()
}

fn raw<'a>(table: &'a SheetTable, a1: &str) -> &'a str {
    at(table, a1).map_or_else(
        || panic!("no cell at {a1} in {table:?}"),
        |cell| cell.raw.as_str(),
    )
}

fn formula<'a>(table: &'a SheetTable, a1: &str) -> Option<&'a str> {
    at(table, a1).and_then(|cell| cell.formula.as_deref())
}

fn format(table: &SheetTable, a1: &str) -> CellFormat {
    at(table, a1)
        .unwrap_or_else(|| panic!("no cell at {a1}"))
        .format
}

const XLSX_CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/worksheets/sheet2.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/><Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/></Types>"#;

const XLSX_ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;

const XLSX_WORKBOOK: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><workbookPr/><sheets><sheet name="Budget" sheetId="1" r:id="rId1"/><sheet name="Notes &amp; Data" sheetId="2" r:id="rId2"/></sheets></workbook>"#;

const XLSX_WORKBOOK_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="/xl/worksheets/sheet2.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/><Relationship Id="rId4" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/></Relationships>"#;

const XLSX_SHARED_STRINGS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="3" uniqueCount="3"><si><t>Item</t></si><si><t>Rent</t></si><si><r><t>Tot</t></r><r><rPr><b/></rPr><t>al</t></r></si></sst>"#;

/// Cell formats: 0 general, 1 built-in date (14), 2 custom date, 3 custom
/// currency, 4 built-in percent (10), 5 built-in duration (46).
const XLSX_STYLES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><numFmts count="2"><numFmt numFmtId="164" formatCode="yyyy\-mm\-dd;@"/><numFmt numFmtId="165" formatCode="&quot;$&quot;#,##0.00"/></numFmts><fonts count="1"><font><sz val="11"/><name val="Calibri"/></font></fonts><fills count="1"><fill><patternFill patternType="none"/></fill></fills><borders count="1"><border/></borders><cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs><cellXfs count="6"><xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/><xf numFmtId="14" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/><xf numFmtId="164" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/><xf numFmtId="165" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/><xf numFmtId="10" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/><xf numFmtId="46" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/></cellXfs></styleSheet>"#;

/// Budget: shared strings (one rich), an inline string, a currency-styled
/// number, built-in and custom date styles, a boolean, a percent, a plain
/// formula, a shared formula filled down three rows, a text-result formula,
/// an error-result formula and a duration.
const XLSX_SHEET1: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1:E6"/><sheetData>
<row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="inlineStr"><is><t>Amount</t></is></c><c r="C1" t="s"><v>2</v></c></row>
<row r="2"><c r="A2" t="s"><v>1</v></c><c r="B2" s="3"><v>1200</v></c><c r="C2"><f t="shared" ref="C2:C4" si="0">B2*2</f><v>2400</v></c><c r="D2" t="e"><f>B2/0</f><v>#DIV/0!</v></c><c r="E2" t="str"><f>IF(B2&gt;100,"big","small")</f><v>big</v></c></row>
<row r="3"><c r="A3" s="1"><v>45000</v></c><c r="B3" s="2"><v>45001.5</v></c><c r="C3"><f t="shared" si="0"/><v>90003</v></c></row>
<row r="4"><c r="A4" t="b"><v>1</v></c><c r="B4" s="4"><v>0.25</v></c><c r="C4"><f t="shared" si="0"/><v>0.5</v></c></row>
<row r="5"><c r="A5"><f>SUM(B2:B4)</f><v>46201.75</v></c></row>
<row r="6"><c r="A6" s="5"><v>0.5</v></c></row>
</sheetData></worksheet>"#;

/// "Notes & Data": anchored at A1 with a gap before C3.
const XLSX_SHEET2: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>note &lt;1&gt;</t></is></c></row><row r="3"><c r="C3"><v>7</v></c></row></sheetData></worksheet>"#;

fn excel_like_xlsx() -> Vec<u8> {
    zip_of(&[
        ("[Content_Types].xml", XLSX_CONTENT_TYPES),
        ("_rels/.rels", XLSX_ROOT_RELS),
        ("xl/workbook.xml", XLSX_WORKBOOK),
        ("xl/_rels/workbook.xml.rels", XLSX_WORKBOOK_RELS),
        ("xl/sharedStrings.xml", XLSX_SHARED_STRINGS),
        ("xl/styles.xml", XLSX_STYLES),
        ("xl/worksheets/sheet1.xml", XLSX_SHEET1),
        ("xl/worksheets/sheet2.xml", XLSX_SHEET2),
    ])
}

/// Excel/Calc imports and XLSX/ODS export round-trips.
#[test]
fn foreign_workbooks_import_and_exports_round_trip() {
    // Case `excel_workbook_imports_tabs_values_formats_and_formulas`.
    {
        let file = TempFile::with("excel.xlsx", &excel_like_xlsx());
        let tables = crate::xlsx::read_xlsx(&file.0).unwrap();
        let names: Vec<&str> = tables.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["Budget", "Notes & Data"]);
        let budget = &tables[0];

        // Shared, rich and inline strings import as text.
        assert_eq!(raw(budget, "A1"), "Item");
        assert_eq!(format(budget, "A1"), CellFormat::Text);
        assert_eq!(raw(budget, "B1"), "Amount");
        assert_eq!(raw(budget, "C1"), "Total");
        assert_eq!(raw(budget, "A2"), "Rent");

        // Numbers import as numbers; only date/time number formats survive as a
        // format (currency and percent re-infer from the value on import).
        assert_eq!(raw(budget, "B2"), "1200");
        assert_eq!(format(budget, "B2"), CellFormat::General);
        assert_eq!(raw(budget, "B4"), "0.25");
        assert_eq!(format(budget, "B4"), CellFormat::General);
        assert_eq!(raw(budget, "A3"), "45000");
        assert_eq!(format(budget, "A3"), CellFormat::Date(DateStyle::Iso));
        assert_eq!(raw(budget, "B3"), "45001.5");
        assert_eq!(format(budget, "B3"), CellFormat::Date(DateStyle::Iso));
        assert_eq!(format(budget, "A6"), CellFormat::Date(DateStyle::Iso));

        assert_eq!(raw(budget, "A4"), "TRUE");
        assert_eq!(format(budget, "A4"), CellFormat::Boolean);

        // Formulas come back live, `=`-prefixed, with their cached results.
        assert_eq!(formula(budget, "A5"), Some("=SUM(B2:B4)"));
        assert_eq!(raw(budget, "A5"), "46201.75");
        assert_eq!(formula(budget, "E2"), Some("=IF(B2>100,\"big\",\"small\")"));
        assert_eq!(raw(budget, "E2"), "big");
        assert_eq!(formula(budget, "D2"), Some("=B2/0"));
        assert_eq!(format(budget, "D2"), CellFormat::Text);
        // An error result keeps Excel's own spelling.
        assert_eq!(raw(budget, "D2"), "#DIV/0!");

        // A shared formula expands to every cell of its fill-down.
        assert_eq!(formula(budget, "C2"), Some("=B2*2"));
        assert_eq!(formula(budget, "C3"), Some("=B3*2"));
        assert_eq!(formula(budget, "C4"), Some("=B4*2"));
        assert_eq!(raw(budget, "C3"), "90003");

        // Blank cells stay blank, and the second tab keeps its gap.
        assert!(at(budget, "B5").is_none());
        let notes = &tables[1];
        assert_eq!(raw(notes, "A1"), "note <1>");
        assert!(at(notes, "B2").is_none());
        assert_eq!(raw(notes, "C3"), "7");
    }

    // Case `calc_spreadsheet_imports_tabs_values_repeats_and_formulas`.
    {
        let file = TempFile::with("calc.ods", &calc_like_ods());
        let tables = crate::ods::read_ods(&file.0).unwrap();
        let names: Vec<&str> = tables.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["Budget", "Other"]);
        let budget = &tables[0];

        assert_eq!(raw(budget, "A1"), "Item");
        assert_eq!(format(budget, "A1"), CellFormat::Text);
        assert_eq!(raw(budget, "B1"), "1200");
        assert_eq!(format(budget, "B1"), CellFormat::General);
        // ODF dates arrive as their ISO text.
        assert_eq!(raw(budget, "A2"), "2024-01-15");
        assert_eq!(format(budget, "A2"), CellFormat::Text);
        assert_eq!(raw(budget, "B2"), "TRUE");
        assert_eq!(format(budget, "B2"), CellFormat::Boolean);
        assert_eq!(raw(budget, "C2"), "0.21");

        // The OpenFormula spelling is normalised to A1.
        assert_eq!(formula(budget, "A3"), Some("=SUM(B1;C2)"));
        assert_eq!(raw(budget, "A3"), "1200.21");
        assert_eq!(raw(budget, "B3"), "line one\nline two");

        // Compressed repeats of populated cells and rows expand.
        for a1 in ["A4", "B4", "C4"] {
            assert_eq!(raw(budget, a1), "0", "{a1}");
        }
        assert_eq!(raw(budget, "A5"), "x");
        assert_eq!(raw(budget, "A6"), "x");
        assert!(at(budget, "B5").is_none());
        // Calc's trailing empty repeat materialises nothing.
        assert!(budget.rows.len() <= 6, "{} rows", budget.rows.len());

        assert_eq!(raw(&tables[1], "A1"), "3.5");
    }

    // Case `sheets_xlsx_export_round_trips`.
    {
        let tables = sheets_workbook();
        let file = TempFile::empty("roundtrip.xlsx");
        crate::xlsx::write_xlsx(&file.0, &tables).unwrap();
        let back = crate::xlsx::read_xlsx(&file.0).unwrap();
        assert_sheets_round_trip(&back);
        // The date format survives the Excel round-trip.
        assert_eq!(format(&back[0], "B4"), CellFormat::Date(DateStyle::Iso));
    }

    // Case `sheets_ods_export_round_trips`.
    {
        let tables = sheets_workbook();
        let file = TempFile::empty("roundtrip.ods");
        crate::ods::write_ods(&file.0, &tables).unwrap();
        let back = crate::ods::read_ods(&file.0).unwrap();
        assert_sheets_round_trip(&back);
    }
}

/// A `content.xml` shaped like Calc's: typed cells with automatic styles,
/// repeated populated cells and rows, a formula, a two-paragraph cell and the
/// huge trailing repeat Calc appends to every table.
const ODS_CONTENT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:of="urn:oasis:names:tc:opendocument:xmlns:of:1.2" office:version="1.3"><office:body><office:spreadsheet>
<table:table table:name="Budget"><table:table-column table:number-columns-repeated="3"/>
<table:table-row><table:table-cell office:value-type="string"><text:p>Item</text:p></table:table-cell><table:table-cell table:style-name="ce1" office:value-type="currency" office:currency="USD" office:value="1200"><text:p>$1,200.00</text:p></table:table-cell></table:table-row>
<table:table-row><table:table-cell office:value-type="date" office:date-value="2024-01-15"><text:p>01/15/24</text:p></table:table-cell><table:table-cell office:value-type="boolean" office:boolean-value="true"><text:p>TRUE</text:p></table:table-cell><table:table-cell office:value-type="percentage" office:value="0.21"><text:p>21.00%</text:p></table:table-cell></table:table-row>
<table:table-row><table:table-cell table:formula="of:=SUM([.B1];[.C2])" office:value-type="float" office:value="1200.21"><text:p>1200.21</text:p></table:table-cell><table:table-cell office:value-type="string"><text:p>line one</text:p><text:p>line two</text:p></table:table-cell></table:table-row>
<table:table-row><table:table-cell office:value-type="float" office:value="0" table:number-columns-repeated="3"><text:p>0</text:p></table:table-cell></table:table-row>
<table:table-row table:number-rows-repeated="2"><table:table-cell office:value-type="string"><text:p>x</text:p></table:table-cell><table:table-cell table:number-columns-repeated="2"/></table:table-row>
<table:table-row table:number-rows-repeated="1048570"><table:table-cell table:number-columns-repeated="1024"/></table:table-row>
</table:table>
<table:table table:name="Other"><table:table-row><table:table-cell office:value-type="float" office:value="3.5"><text:p>3.5</text:p></table:table-cell></table:table-row></table:table>
</office:spreadsheet></office:body></office:document-content>"#;

const ODS_MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0" manifest:version="1.3"><manifest:file-entry manifest:full-path="/" manifest:media-type="application/vnd.oasis.opendocument.spreadsheet"/><manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/></manifest:manifest>"#;

fn calc_like_ods() -> Vec<u8> {
    zip_of(&[
        ("mimetype", "application/vnd.oasis.opendocument.spreadsheet"),
        ("META-INF/manifest.xml", ODS_MANIFEST),
        ("content.xml", ODS_CONTENT),
    ])
}

/// A workbook as Sheets exports it: every format, a formula of each result
/// type, several tabs.
fn sheets_workbook() -> Vec<SheetTable> {
    use crate::model::{Addr, CellValue};
    use std::collections::HashMap;

    let mut raw: HashMap<Addr, String> = HashMap::new();
    let mut fmt: HashMap<Addr, CellFormat> = HashMap::new();
    raw.insert((0, 0), "Item".into());
    raw.insert((1, 0), "Cost".into());
    raw.insert((0, 1), "Rent".into());
    raw.insert((1, 1), "1200".into());
    fmt.insert((1, 1), CellFormat::Currency(Currency::EUR));
    raw.insert((0, 2), "Rate".into());
    raw.insert((1, 2), "0.21".into());
    fmt.insert((1, 2), CellFormat::Percent);
    raw.insert((0, 3), "Due".into());
    raw.insert((1, 3), "45000".into());
    fmt.insert((1, 3), CellFormat::Date(DateStyle::Iso));
    raw.insert((0, 4), "Paid".into());
    raw.insert((1, 4), "TRUE".into());
    fmt.insert((1, 4), CellFormat::Boolean);
    raw.insert((0, 5), "007".into());
    fmt.insert((0, 5), CellFormat::Text);
    raw.insert((1, 5), "=B2*(1+B3)".into());
    raw.insert((2, 5), "=IF(B6>1000,\"high\",\"low\")".into());
    raw.insert((3, 7), "far".into());
    let values: HashMap<Addr, CellValue> = crate::formula::recalculate(&raw);
    vec![
        SheetTable::from_cells(
            "Budget 2026",
            &raw,
            &fmt,
            &values,
            &std::collections::HashSet::new(),
        ),
        SheetTable::from_cells(
            "Empty",
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &std::collections::HashSet::new(),
        ),
        SheetTable::from_cells(
            "Q1",
            &HashMap::from([((0, 0), "1".into())]),
            &HashMap::new(),
            &HashMap::new(),
            &std::collections::HashSet::new(),
        ),
    ]
}

fn assert_sheets_round_trip(back: &[SheetTable]) {
    let names: Vec<&str> = back.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["Budget 2026", "Empty", "Q1"]);
    let budget = &back[0];
    assert_eq!(raw(budget, "A1"), "Item");
    assert_eq!(raw(budget, "B2"), "1200");
    assert!((raw(budget, "B3").parse::<f64>().unwrap() - 0.21).abs() < 1e-9);
    assert_eq!(raw(budget, "B4"), "45000");
    assert_eq!(raw(budget, "B5"), "TRUE");
    assert_eq!(format(budget, "B5"), CellFormat::Boolean);
    // A text-formatted cell is written and read back as text (its payload is
    // the computed value, as for every exported cell).
    assert_eq!(raw(budget, "A6"), "7");
    assert_eq!(format(budget, "A6"), CellFormat::Text);
    assert_eq!(formula(budget, "B6"), Some("=B2*(1+B3)"));
    assert!((raw(budget, "B6").parse::<f64>().unwrap() - 1452.0).abs() < 1e-9);
    assert_eq!(formula(budget, "C6"), Some("=IF(B6>1000,\"high\",\"low\")"));
    assert_eq!(raw(budget, "C6"), "high");
    assert_eq!(raw(budget, "D8"), "far");
    assert!(at(budget, "C8").is_none());
    assert_eq!(raw(&back[2], "A1"), "1");
}

/// A one-tab `.xlsx` with the given worksheet body.
fn xlsx_sheet(sheet_xml: &str) -> Vec<u8> {
    let workbook = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="S" sheetId="1" r:id="rId1"/></sheets></workbook>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#;
    zip_of(&[
        ("[Content_Types].xml", XLSX_CONTENT_TYPES),
        ("_rels/.rels", XLSX_ROOT_RELS),
        ("xl/workbook.xml", workbook),
        ("xl/_rels/workbook.xml.rels", rels),
        ("xl/worksheets/sheet1.xml", sheet_xml),
    ])
}

/// Offset tabs keep positions; sparse tabs and DTD packages are refused.
#[test]
fn offset_sparse_and_dtd_packages_are_handled_safely() {
    // Case `a_tab_that_starts_below_a1_keeps_its_cell_positions`.
    {
        let file = TempFile::with(
            "offset.xlsx",
            &xlsx_sheet(
                r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="3"><c r="C3"><v>1</v></c><c r="D3"><f>C3*2</f><v>2</v></c></row></sheetData></worksheet>"#,
            ),
        );
        let tables = crate::xlsx::read_xlsx(&file.0).unwrap();
        assert!(at(&tables[0], "A1").is_none());
        assert_eq!(raw(&tables[0], "C3"), "1");
        // The formula still points at the cell it was written against.
        assert_eq!(formula(&tables[0], "D3"), Some("=C3*2"));
    }

    // Case `a_far_flung_sparse_tab_is_refused_before_it_is_materialised`.
    {
        let file = TempFile::with(
            "far.xlsx",
            &xlsx_sheet(
                r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1"><v>1</v></c></row><row r="1048576"><c r="XFD1048576"><v>2</v></c></row></sheetData></worksheet>"#,
            ),
        );
        let error = crate::xlsx::read_xlsx(&file.0).unwrap_err();
        assert!(error.to_string().contains("import limit"), "{error:#}");
    }

    // Case `a_package_carrying_a_dtd_is_refused`.
    {
        let xxe = r#"<?xml version="1.0"?><!DOCTYPE w [<!ENTITY x SYSTEM "file:///etc/passwd">]><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>&x;</t></is></c></row></sheetData></worksheet>"#;
        let file = TempFile::with("xxe.xlsx", &xlsx_sheet(xxe));
        let error = crate::xlsx::read_xlsx(&file.0).unwrap_err();
        assert!(error.to_string().contains("DTD"), "{error:#}");

        let content = ODS_CONTENT.replacen(
            "<office:document-content",
            r#"<!DOCTYPE d [<!ENTITY x "boom">]><office:document-content"#,
            1,
        );
        let file = TempFile::with(
            "xxe.ods",
            &zip_of(&[
                ("mimetype", "application/vnd.oasis.opendocument.spreadsheet"),
                ("META-INF/manifest.xml", ODS_MANIFEST),
                ("content.xml", &content),
            ]),
        );
        let error = crate::ods::read_ods(&file.0).unwrap_err();
        assert!(error.to_string().contains("DTD"), "{error:#}");
    }
}
