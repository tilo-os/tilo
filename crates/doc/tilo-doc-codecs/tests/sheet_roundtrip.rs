//! Real-path spreadsheet round-trip, security, and performance tests over genuine
//! `.xlsx`/`.ods` packages (built here as complete OOXML/ODF ZIPs — the same bytes
//! Excel/LibreOffice write). Each is parsed into the `qdoc` CRDT [`Workbook`]
//! model, serialized back, and reopened — proving the model is the source of truth
//! and that **formulas are preserved, not flattened to their cached value**.

#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::fmt::Write as _;
use std::io::{Cursor, Read, Write};
use tilo_doc::sheet::snapshot::CellType;
use tilo_doc::{CellInput, CellView, Workbook, WorkbookSnapshot};
use tilo_doc_codecs::{OdsCodec, SheetProvider, XlsxCodec};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

/// Build a ZIP (deflated) from `(name, bytes)` entries — a genuine OPC/ODF package.
#[allow(
    clippy::unwrap_used,
    reason = "test fixture builder: a failure here means the test infra itself is broken, not the code under test"
)]
fn zip_of(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut zw = ZipWriter::new(Cursor::new(Vec::new()));
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (name, data) in entries {
        zw.start_file(*name, opts).unwrap();
        zw.write_all(data).unwrap();
    }
    zw.finish().unwrap().into_inner()
}

#[allow(
    clippy::expect_used,
    reason = "test fixture helper: a failure here is a broken fixture, and the message says so"
)]
fn imp(codec: &dyn SheetProvider, bytes: &[u8]) -> Workbook {
    let mut wb = Workbook::new();
    codec
        .import(bytes, &mut wb)
        .expect("import into workbook model");
    wb
}

/// A real two-sheet `.xlsx` exercising every modeled cell kind: a number, an
/// inline string, a formula (with cached result), and a **shared string** (t="s"
/// into sharedStrings.xml) — so the sst-resolution path is covered.
fn sample_xlsx() -> Vec<u8> {
    let content_types = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/worksheets/sheet2.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/></Types>"#.to_vec();
    let root_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#.to_vec();
    let workbook = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Budget" sheetId="1" r:id="rId1"/><sheet name="Data" sheetId="2" r:id="rId2"/></sheets></workbook>"#.to_vec();
    let wb_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet2.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/></Relationships>"#.to_vec();
    // Budget: A1 number 10, B1 inline string "Hello", A2 formula =A1*2 (cached 20).
    let sheet1 = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1"><v>10</v></c><c r="B1" t="inlineStr"><is><t>Hello</t></is></c></row><row r="2"><c r="A2"><f>A1*2</f><v>20</v></c></row></sheetData></worksheet>"#.to_vec();
    // Data: A1 shared string idx 0 ("Region"), B1 number 42, C1 formula =B1+1 (cached 43).
    let sheet2 = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1"><v>42</v></c><c r="C1"><f>B1+1</f><v>43</v></c></row></sheetData></worksheet>"#.to_vec();
    let sst = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="1" uniqueCount="1"><si><t>Region</t></si></sst>"#.to_vec();
    zip_of(&[
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", root_rels),
        ("xl/workbook.xml", workbook),
        ("xl/_rels/workbook.xml.rels", wb_rels),
        ("xl/worksheets/sheet1.xml", sheet1),
        ("xl/worksheets/sheet2.xml", sheet2),
        ("xl/sharedStrings.xml", sst),
    ])
}

fn sample_ods() -> Vec<u8> {
    let content = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"><office:body><office:spreadsheet><table:table table:name="Budget"><table:table-column table:number-columns-repeated="2"/><table:table-row><table:table-cell office:value-type="float" office:value="10"><text:p>10</text:p></table:table-cell><table:table-cell office:value-type="string"><text:p>Hello</text:p></table:table-cell></table:table-row><table:table-row><table:table-cell office:value-type="float" office:value="20" table:formula="of:=[.A1]*2"><text:p>20</text:p></table:table-cell></table:table-row></table:table><table:table table:name="Data"><table:table-row><table:table-cell office:value-type="string"><text:p>Region</text:p></table:table-cell><table:table-cell office:value-type="float" office:value="42"><text:p>42</text:p></table:table-cell><table:table-cell office:value-type="float" office:value="43" table:formula="of:=[.B1]+1"><text:p>43</text:p></table:table-cell></table:table-row></table:table></office:spreadsheet></office:body></office:document-content>"#.to_vec();
    let manifest = br#"<?xml version="1.0" encoding="UTF-8"?>
<manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0"><manifest:file-entry manifest:full-path="/" manifest:media-type="application/vnd.oasis.opendocument.spreadsheet"/><manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/></manifest:manifest>"#.to_vec();
    zip_of(&[
        (
            "mimetype",
            b"application/vnd.oasis.opendocument.spreadsheet".to_vec(),
        ),
        ("content.xml", content),
        ("META-INF/manifest.xml", manifest),
    ])
}

/// The invariant both fixtures encode: two sheets (Budget, Data), a number, an
/// inline/plain string, and TWO formulas whose sources survive verbatim.
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertion helper: a panic here means a fixture bug, and the message is the fixture's context"
)]
fn assert_sample(snap: &WorkbookSnapshot, fmt: &str, a2_formula: &str, formula_c1: &str) {
    let names: Vec<&str> = snap.sheets.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["Budget", "Data"],
        "{fmt}: two sheets in tab order"
    );

    let budget = snap.sheet_by_name("Budget").unwrap();
    let a1 = budget
        .cell("A1")
        .unwrap_or_else(|| panic!("{fmt}: Budget!A1 missing"));
    assert_eq!(
        (a1.value.as_str(), a1.cell_type),
        ("10", CellType::Number),
        "{fmt}: A1 number"
    );
    let b1 = budget.cell("B1").unwrap();
    assert_eq!(
        (b1.value.as_str(), b1.cell_type),
        ("Hello", CellType::Text),
        "{fmt}: B1 string"
    );
    let a2 = budget.cell("A2").unwrap();
    assert_eq!(
        a2.formula.as_deref(),
        Some(a2_formula),
        "{fmt}: A2 formula preserved (not flattened)"
    );
    assert_eq!(a2.value, "20", "{fmt}: A2 cached result");

    let data = snap.sheet_by_name("Data").unwrap();
    assert_eq!(
        data.cell("A1").unwrap().value,
        "Region",
        "{fmt}: Data!A1 (shared) string"
    );
    assert_eq!(
        data.cell("B1").unwrap().value,
        "42",
        "{fmt}: Data!B1 number"
    );
    assert_eq!(
        data.cell("C1").unwrap().formula.as_deref(),
        Some(formula_c1),
        "{fmt}: Data!C1 formula preserved"
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn real_spreadsheets_parse_roundtrip_and_edit_as_crdt() {
    // Phase `xlsx_parses_real_file_into_crdt_model`.
    {
        let wb = imp(&XlsxCodec, &sample_xlsx());
        assert_sample(&wb.materialize(), "xlsx", "A1*2", "B1+1");
    }
    // Phase `ods_parses_real_file_into_crdt_model`.
    {
        let wb = imp(&OdsCodec, &sample_ods());
        assert_sample(&wb.materialize(), "ods", "of:=[.A1]*2", "of:=[.B1]+1");
    }
    // Phase `xlsx_roundtrips_cells_types_and_formulas`.
    {
        let wb = imp(&XlsxCodec, &sample_xlsx());
        let out = XlsxCodec.export(&wb).expect("export xlsx");
        let reopened = imp(&XlsxCodec, &out);
        assert_sample(&reopened.materialize(), "xlsx-reopened", "A1*2", "B1+1");
    }
    // Phase `ods_roundtrips_cells_types_and_formulas`.
    {
        let wb = imp(&OdsCodec, &sample_ods());
        let out = OdsCodec.export(&wb).expect("export ods");
        let reopened = imp(&OdsCodec, &out);
        assert_sample(
            &reopened.materialize(),
            "ods-reopened",
            "of:=[.A1]*2",
            "of:=[.B1]+1",
        );
    }
    // Phase `xlsx_edit_as_crdt_op_survives_reopen`.
    {
        let mut wb = imp(&XlsxCodec, &sample_xlsx());
        let budget = wb.sheet_ids_in_order()[0].clone();
        wb.set_cell(&budget, "A1", CellInput::number("15"));
        wb.set_cell(
            &budget,
            "C3",
            CellInput::formula("SUM(A1:A2)", "35", CellType::Number),
        );
        let versions_before = wb.versions().len();
        assert!(versions_before > 1, "edits extend the CRDT change history");

        let out = XlsxCodec.export(&wb).unwrap();
        let snap = imp(&XlsxCodec, &out).materialize();
        let b = snap.sheet_by_name("Budget").unwrap();
        assert_eq!(
            b.cell("A1").unwrap().value,
            "15",
            "edited cell survived reopen"
        );
        assert_eq!(
            b.cell("C3").unwrap().formula.as_deref(),
            Some("SUM(A1:A2)"),
            "new formula survived reopen"
        );
        assert_eq!(b.cell("B1").unwrap().value, "Hello");
    }
    // Phase `xlsx_keeps_theme_backed_styles_and_declares_minimal_styles`.
    {
        let mut entries = Vec::new();
        // Start from a genuine workbook, then add the pair Office uses for
        // theme-coloured style records. The export must retain both the part and
        // the relationship — retaining styles.xml alone changes its appearance.
        let mut source = ZipArchive::new(Cursor::new(sample_xlsx())).unwrap();
        for i in 0..source.len() {
            let mut file = source.by_index(i).unwrap();
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).unwrap();
            entries.push((file.name().unwrap().into_owned(), bytes));
        }
        entries.push((
            "xl/styles.xml".into(),
            br#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><fonts count="1"><font><color theme="1"/></font></fonts><fills count="1"><fill><patternFill patternType="none"/></fill></fills><borders count="1"><border/></borders><cellStyleXfs count="1"><xf/></cellStyleXfs><cellXfs count="1"><xf fontId="0"/></cellXfs></styleSheet>"#.to_vec(),
        ));
        entries.push((
            "xl/theme/theme1.xml".into(),
            br#"<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" name="Brand"><a:themeElements/></a:theme>"#.to_vec(),
        ));
        let refs: Vec<(&str, Vec<u8>)> = entries
            .iter()
            .map(|(n, b)| (n.as_str(), b.clone()))
            .collect();
        let wb = imp(&XlsxCodec, &zip_of(&refs));
        let out = XlsxCodec.export(&wb).unwrap();
        let mut exported = ZipArchive::new(Cursor::new(out)).unwrap();
        assert!(exported.by_name("xl/styles.xml").is_ok());
        assert!(exported.by_name("xl/theme/theme1.xml").is_ok());
        let mut relationships_xml = String::new();
        exported
            .by_name("xl/_rels/workbook.xml.rels")
            .unwrap()
            .read_to_string(&mut relationships_xml)
            .unwrap();
        assert!(relationships_xml.contains("relationships/theme"));
        let mut types = String::new();
        exported
            .by_name("[Content_Types].xml")
            .unwrap()
            .read_to_string(&mut types)
            .unwrap();
        assert!(types.contains("spreadsheetml.styles+xml"));
        assert!(types.contains("officedocument.theme+xml"));
    }
    // Phase `entity_references_survive_in_strings_formulas_and_cell_text`.
    {
        let sheet = r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>R&amp;D &lt;1&gt;</t></is></c><c r="B1" t="str"><f>IF(A2&gt;1,&quot;a&amp;b&quot;,&#39;x&#39;)</f><v>a&amp;b</v></c></row></sheetData></worksheet>"#;
        let parsed = XlsxCodec::parse(&xlsx_with(sheet, None)).unwrap();
        let cells = &parsed.sheets[0].cells;
        assert_eq!(cells[0].1.value, "R&D <1>");
        assert_eq!(cells[1].1.formula.as_deref(), Some(r#"IF(A2>1,"a&b",'x')"#));
        assert_eq!(cells[1].1.value, "a&b");

        let table = r#"<table:table table:name="S"><table:table-row><table:table-cell office:value-type="string"><text:p>Fish &amp; chips<text:s text:c="3"/>&#x263A;<text:tab/>end</text:p></table:table-cell></table:table-row></table:table>"#;
        let parsed = OdsCodec::parse(&ods_with(table)).unwrap();
        assert_eq!(
            parsed.sheets[0].cells[0].1.value,
            "Fish & chips   \u{263A}\tend"
        );
    }
}

#[test]
fn hostile_spreadsheet_inputs_are_rejected() {
    // Phase `xxe_dtd_is_rejected_in_both_spreadsheet_formats`.
    {
        // A DTD in a modeled XML part is rejected outright (defence in depth vs XXE /
        // billion-laughs), via the same codecs/safety.rs guard the doc codecs use.
        let evil_wb = br#"<?xml version="1.0"?><!DOCTYPE workbook [<!ENTITY x "boom">]><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheets><sheet name="S" sheetId="1" r:id="rId1"/></sheets></workbook>"#.to_vec();
        let xlsx = zip_of(&[
            ("[Content_Types].xml", b"<Types/>".to_vec()),
            ("xl/workbook.xml", evil_wb),
        ]);
        let mut wb = Workbook::new();
        let err = XlsxCodec
            .import(&xlsx, &mut wb)
            .expect_err("xlsx XXE must be rejected");
        assert!(
            format!("{err}").contains("DTD"),
            "xlsx: expected a DTD rejection, got: {err}"
        );

        let evil_content = br#"<?xml version="1.0"?><!DOCTYPE office:document-content [<!ENTITY x "boom">]><office:document-content/>"#.to_vec();
        let ods = zip_of(&[
            (
                "mimetype",
                b"application/vnd.oasis.opendocument.spreadsheet".to_vec(),
            ),
            ("content.xml", evil_content),
        ]);
        let mut wb2 = Workbook::new();
        let err2 = OdsCodec
            .import(&ods, &mut wb2)
            .expect_err("ods XXE must be rejected");
        assert!(
            format!("{err2}").contains("DTD"),
            "ods: expected a DTD rejection, got: {err2}"
        );
    }
    // Phase `zip_bomb_is_rejected_in_both_spreadsheet_formats`.
    {
        // An entry-count amplification bomb (a few KB → thousands of parts, each read
        // into memory) is rejected fast by the bounded reader, before any part is
        // modeled — the same ZipLimits cap the doc codecs enforce.
        let many: Vec<(String, Vec<u8>)> = (0..9000)
            .map(|i| (format!("p{i}.xml"), b"<x/>".to_vec()))
            .collect();
        let refs: Vec<(&str, Vec<u8>)> =
            many.iter().map(|(n, b)| (n.as_str(), b.clone())).collect();
        let bomb = zip_of(&refs);

        let mut wb = Workbook::new();
        let err = XlsxCodec
            .import(&bomb, &mut wb)
            .expect_err("xlsx zip bomb must be rejected");
        assert!(
            format!("{err}").contains("limit") || format!("{err}").contains("bomb"),
            "xlsx: expected a bounded rejection, got: {err}"
        );

        let mut wb2 = Workbook::new();
        let err2 = OdsCodec
            .import(&bomb, &mut wb2)
            .expect_err("ods zip bomb must be rejected");
        assert!(
            format!("{err2}").contains("limit") || format!("{err2}").contains("bomb"),
            "ods: expected a bounded rejection, got: {err2}"
        );
    }
    // Phase `garbage_input_is_rejected_cleanly`.
    {
        let mut wb = Workbook::new();
        assert!(XlsxCodec.import(b"not a zip at all", &mut wb).is_err());
        assert!(OdsCodec.import(&[0xff, 0x00, 0x13, 0x37], &mut wb).is_err());
    }
    // Phase `ods_refuses_a_repeat_that_claims_too_many_cells`.
    {
        let table = r#"<table:table table:name="S"><table:table-row table:number-rows-repeated="1048576"><table:table-cell office:value-type="float" office:value="1" table:number-columns-repeated="16384"><text:p>1</text:p></table:table-cell></table:table-row></table:table>"#;
        let err = OdsCodec::parse(&ods_with(table)).unwrap_err();
        assert!(format!("{err}").contains("repeated cells"), "got {err}");
    }
}

/// A workbook of several sheets × hundreds of formula cells, exported +
/// reimported for both formats under a generous bound — a guard against a future
/// super-linear regression creeping into the cell path (materialize sort, XML
/// build, parse). The size is bounded so it stays fast in a debug `cargo test`;
/// automerge's per-object insertion cost is the ceiling on incremental cell
/// creation, so this measures the codec path around it, not automerge itself.
#[test]
fn large_workbook_parse_and_serialize_is_fast() {
    use std::time::Instant;

    const SHEETS: usize = 4;
    const ROWS: u32 = 50;
    let mut wb = Workbook::new();
    for s in 0..SHEETS {
        let id = wb.add_sheet(&format!("Sheet{s}"));
        for r in 1..=ROWS {
            let reference = format!("A{r}");
            // Every cell is a formula whose source must survive.
            wb.set_cell(
                &id,
                &reference,
                CellInput::formula(format!("B{r}+{r}"), format!("{}", r + 1), CellType::Number),
            );
        }
    }
    let total = SHEETS.saturating_mul(usize::try_from(ROWS).unwrap_or(usize::MAX));
    assert_eq!(
        wb.materialize()
            .sheets
            .iter()
            .map(|s| s.cells.len())
            .sum::<usize>(),
        total,
        "built all cells"
    );

    let mut report = String::new();
    for (name, codec) in [
        ("xlsx", &XlsxCodec as &dyn SheetProvider),
        ("ods", &OdsCodec as &dyn SheetProvider),
    ] {
        let t0 = Instant::now();
        let bytes = codec.export(&wb).expect("export large workbook");
        let export_ms = t0.elapsed().as_secs_f64() * 1000.0;

        let t1 = Instant::now();
        let round = imp(codec, &bytes);
        let import_ms = t1.elapsed().as_secs_f64() * 1000.0;

        let snap = round.materialize();
        assert_eq!(snap.sheets.len(), SHEETS, "{name}: sheet count preserved");
        assert_eq!(
            snap.sheets.iter().map(|s| s.cells.len()).sum::<usize>(),
            total,
            "{name}: cell count preserved"
        );
        // Spot-check a formula survived the whole round-trip.
        let last = snap
            .sheets
            .last()
            .unwrap()
            .cell(&format!("A{ROWS}"))
            .unwrap();
        assert_eq!(
            last.formula.as_deref(),
            Some(format!("B{ROWS}+{ROWS}").as_str()),
            "{name}: formula preserved end-to-end"
        );

        // Display-only byte count; a test workbook is nowhere near f64's exact-integer range.
        #[allow(
            clippy::cast_precision_loss,
            reason = "display-only byte count, far inside f64's exact-integer range"
        )]
        let kib = bytes.len() as f64 / 1024.0;
        let _ = writeln!(
            report,
            "  {name:>4}: export {export_ms:8.1} ms, reimport {import_ms:8.1} ms, {kib:8.1} KiB"
        );
        // Wall-clock is release-gated: a cell is an automerge sub-map, so building the
        // grid is bounded by automerge-0.8's inherent O(n^2) object creation in a debug
        // build, not by the codec. Cell/type/formula preservation above is asserted in
        // every mode; the timing guard runs where it is meaningful.
        if !cfg!(debug_assertions) {
            assert!(
                export_ms < 15_000.0,
                "{name}: export too slow ({export_ms:.0} ms) — possible O(n^2)"
            );
            assert!(
                import_ms < 15_000.0,
                "{name}: reimport too slow ({import_ms:.0} ms) — possible O(n^2)"
            );
        }
    }
    eprintln!("large_workbook_parse_and_serialize ({total} cells):\n{report}");
}

/// A one-sheet `.xlsx` whose worksheet XML and (optional) styles are given.
fn xlsx_with(sheet_xml: &str, styles_xml: Option<&str>) -> Vec<u8> {
    let workbook = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="S" sheetId="1" r:id="rId1"/></sheets></workbook>"#.to_vec();
    let wb_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#.to_vec();
    let mut entries = vec![
        ("xl/workbook.xml", workbook),
        ("xl/_rels/workbook.xml.rels", wb_rels),
        ("xl/worksheets/sheet1.xml", sheet_xml.as_bytes().to_vec()),
    ];
    if let Some(styles) = styles_xml {
        entries.push(("xl/styles.xml", styles.as_bytes().to_vec()));
    }
    zip_of(&entries)
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn spreadsheet_formulas_formats_and_repeats_expand_and_export() {
    // Phase `xlsx_shared_formulas_expand_to_every_cell_of_their_range`.
    {
        let sheet = r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>
    <row r="1"><c r="A1"><v>1</v></c><c r="B1"><f t="shared" ref="B1:B3" si="0">A1*$A$1+SUM(A$1:A1)</f><v>2</v></c></row>
    <row r="2"><c r="A2"><v>2</v></c><c r="B2"><f t="shared" si="0"/><v>5</v></c></row>
    <row r="3"><c r="A3"><v>3</v></c><c r="B3"><f t="shared" si="0"></f><v>9</v></c><c r="C3"><f t="shared" si="7"/><v>4</v></c></row>
    </sheetData></worksheet>"#;
        let parsed = XlsxCodec::parse(&xlsx_with(sheet, None)).unwrap();
        let cells: std::collections::HashMap<&str, &CellInput> = parsed.sheets[0]
            .cells
            .iter()
            .map(|(r, c)| (r.as_str(), c))
            .collect();
        assert_eq!(cells["B1"].formula.as_deref(), Some("A1*$A$1+SUM(A$1:A1)"));
        assert_eq!(cells["B2"].formula.as_deref(), Some("A2*$A$1+SUM(A$1:A2)"));
        assert_eq!(cells["B3"].formula.as_deref(), Some("A3*$A$1+SUM(A$1:A3)"));
        assert_eq!(cells["B3"].value, "9");
        // A dependent whose anchor never appears keeps only its cached value.
        assert_eq!(cells["C3"].formula, None);
        assert_eq!(cells["C3"].value, "4");

        // The CRDT import sees the same formulas, and they survive export.
        let wb = imp(&XlsxCodec, &xlsx_with(sheet, None));
        let back = imp(&XlsxCodec, &XlsxCodec.export(&wb).unwrap()).materialize();
        assert_eq!(
            back.sheets[0].cell("B3").unwrap().formula.as_deref(),
            Some("A3*$A$1+SUM(A$1:A3)")
        );
    }
    // Phase `xlsx_date_style_refs_follow_the_cell_formats_number_format`.
    {
        let styles = r#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><numFmts count="4"><numFmt numFmtId="164" formatCode="yyyy\-mm\-dd;@"/><numFmt numFmtId="165" formatCode="&quot;$&quot;#,##0.00"/><numFmt numFmtId="166" formatCode="[h]:mm:ss"/><numFmt numFmtId="167" formatCode="[Red]0.00;&quot;day&quot;"/></numFmts><cellStyleXfs count="1"><xf numFmtId="14"/></cellStyleXfs><cellXfs count="8"><xf numFmtId="0"/><xf numFmtId="14"/><xf numFmtId="164"></xf><xf numFmtId="165"/><xf numFmtId="10"/><xf numFmtId="166"/><xf numFmtId="167"/><xf numFmtId="46"/></cellXfs></styleSheet>"#;
        let refs = tilo_doc_codecs::xlsx::date_style_refs(styles.as_bytes());
        let refs: Vec<&str> = refs.iter().map(String::as_str).collect();
        assert_eq!(refs, ["1", "2", "5", "7"]);
    }
    // Phase `xlsx_snapshot_export_writes_number_formats_and_valid_tab_names`.
    {
        use tilo_doc_codecs::xlsx::{date_style_refs, number_format_styles};
        let styles = number_format_styles(&["yyyy-mm-dd", "#,##0.00 \"CHF\""]);
        assert_eq!(
            date_style_refs(&styles).into_iter().collect::<Vec<_>>(),
            ["1"]
        );
        let snap = snapshot_of(vec![
            (
                "Q1/Q2",
                vec![
                    number_cell("A1", "45000", Some("1")),
                    number_cell("B1", "12.5", Some("2")),
                ],
            ),
            ("q1?q2", vec![]),
            ("", vec![]),
        ]);
        let preserved = std::collections::BTreeMap::from([("xl/styles.xml".to_string(), styles)]);
        let bytes = XlsxCodec::export_snapshot(&snap, &preserved).unwrap();
        let parsed = XlsxCodec::parse(&bytes).unwrap();
        let names: Vec<&str> = parsed.sheets.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Q1_Q2", "q1_q2 (2)", "Sheet"]);
        let a1 = &parsed.sheets[0].cells[0].1;
        assert_eq!(
            (a1.value.as_str(), a1.style.as_deref()),
            ("45000", Some("1"))
        );
        let dates = date_style_refs(&parsed.preserved["xl/styles.xml"]);
        assert!(dates.contains("1") && !dates.contains("2"));
    }
    // Phase `ods_repeated_populated_cells_and_rows_expand`.
    {
        let table = r#"<table:table table:name="S">
    <table:table-row><table:table-cell office:value-type="float" office:value="0" table:number-columns-repeated="3"><text:p>0</text:p></table:table-cell><table:table-cell table:style-name="ce1" table:number-columns-repeated="1000"/></table:table-row>
    <table:table-row table:number-rows-repeated="2"><table:table-cell office:value-type="string"><text:p>x</text:p></table:table-cell></table:table-row>
    <table:table-row table:number-rows-repeated="1048570"><table:table-cell table:style-name="ce2" table:number-columns-repeated="1024"/></table:table-row>
    </table:table>"#;
        let parsed = OdsCodec::parse(&ods_with(table)).unwrap();
        let refs: Vec<&str> = parsed.sheets[0]
            .cells
            .iter()
            .map(|(r, _)| r.as_str())
            .collect();
        // Style-only runs are recorded once; populated runs fill every position.
        assert_eq!(refs, ["A1", "B1", "C1", "D1", "A2", "A3", "A4"]);
        assert_eq!(parsed.sheets[0].cells[4].1.value, "x");
        assert_eq!(parsed.sheets[0].cells[4].1, parsed.sheets[0].cells[5].1);
    }
    // Phase `ods_snapshot_export_carries_common_styles`.
    {
        let styles = tilo_doc_codecs::ods::common_styles_part(
            r#"<number:percentage-style style:name="pct"><number:number number:decimal-places="2"/><number:text>%</number:text></number:percentage-style><style:style style:name="coPct" style:family="table-cell" style:data-style-name="pct"/>"#,
        );
        let mut cell = number_cell("A1", "0.25", Some("coPct"));
        cell.formula = Some("of:=[.B1]/4".into());
        let snap = snapshot_of(vec![("S", vec![cell])]);
        let preserved =
            std::collections::BTreeMap::from([("styles.xml".to_string(), styles.clone())]);
        let bytes = OdsCodec::export_snapshot(&snap, &preserved).unwrap();
        let parsed = OdsCodec::parse(&bytes).unwrap();
        assert_eq!(parsed.preserved["styles.xml"], styles);
        let a1 = &parsed.sheets[0].cells[0].1;
        assert_eq!(a1.style.as_deref(), Some("coPct"));
        assert_eq!(a1.formula.as_deref(), Some("of:=[.B1]/4"));
        let mut archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut manifest = String::new();
        archive
            .by_name("META-INF/manifest.xml")
            .unwrap()
            .read_to_string(&mut manifest)
            .unwrap();
        assert!(manifest.contains(r#"manifest:full-path="styles.xml""#));
    }
}

fn number_cell(reference: &str, value: &str, style: Option<&str>) -> CellView {
    let (col, row) = tilo_doc::sheet::a1::parse_ref(reference).unwrap();
    CellView {
        reference: reference.to_string(),
        col,
        row,
        value: value.to_string(),
        cell_type: CellType::Number,
        formula: None,
        style: style.map(str::to_string),
    }
}

fn snapshot_of(sheets: Vec<(&str, Vec<CellView>)>) -> WorkbookSnapshot {
    WorkbookSnapshot {
        schema: String::new(),
        schema_version: 1,
        sheets: sheets
            .into_iter()
            .map(|(name, cells)| tilo_doc::SheetView {
                id: name.to_string(),
                name: name.to_string(),
                cells,
            })
            .collect(),
        properties: std::collections::BTreeMap::new(),
    }
}

fn ods_with(table_xml: &str) -> Vec<u8> {
    let content = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"><office:body><office:spreadsheet>{table_xml}</office:spreadsheet></office:body></office:document-content>"#
    );
    zip_of(&[
        (
            "mimetype",
            b"application/vnd.oasis.opendocument.spreadsheet".to_vec(),
        ),
        ("content.xml", content.into_bytes()),
    ])
}
