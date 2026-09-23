//! `.ods` (`OpenDocument` Spreadsheet) codec. Parses the ODF package's `content.xml`
//! spreadsheet body into the CRDT [`Workbook`] model — sheets, positional cells,
//! cell types, and **formulas** (`table:formula`, preserved verbatim) — and
//! serializes a complete valid package back. Untrusted bytes enter only through
//! the bounded/hardened [`SheetPackage`] reader (zip-bomb + XXE defence).

use crate::codecs::sheet_common::{
    SheetPackage, SheetProvider, attr_local, end_local_name, local_name,
};
use crate::codecs::xml_escape::escape_attr_quotes;
use crate::format::{CodecError, Confidence};
use crate::sheet::snapshot::CellType;
use crate::sheet::{CellInput, CellView, SheetView, Workbook, WorkbookSnapshot};
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use std::collections::BTreeMap;
// `write!` into a `String` cannot fail; the discarded `Result` is the
// `fmt::Write` contract, not a swallowed error.
use std::fmt::Write as _;

const MIMETYPE: &str = "application/vnd.oasis.opendocument.spreadsheet";

pub struct OdsCodec;

impl SheetProvider for OdsCodec {
    fn schema_id(&self) -> &str {
        crate::sheet::schema::SHEET_SCHEMA_ID
    }

    fn detect(&self, head: &[u8]) -> Confidence {
        if head.starts_with(b"PK") {
            Confidence::Low
        } else {
            Confidence::None
        }
    }

    fn import(&self, bytes: &[u8], wb: &mut Workbook) -> Result<(), CodecError> {
        let pkg = SheetPackage::read(bytes)?;
        let content = pkg
            .get("content.xml")
            .ok_or_else(|| CodecError::Parse("ods: missing content.xml".into()))?;
        parse_content(content, wb)?;
        if let Some(meta) = pkg.get("meta.xml") {
            import_meta(meta, wb);
        }
        // Preserve styles + other unmodeled parts so `table:style-name` refs resolve; content.xml is regenerated on export.
        for name in pkg.names() {
            if is_preserved_part(name)
                && let Some(b) = pkg.get(name)
            {
                wb.set_preserved(&format!("ods/part/{name}"), b);
            }
        }
        Ok(())
    }

    fn export(&self, wb: &Workbook) -> Result<Vec<u8>, CodecError> {
        let snap = wb.materialize();
        let mut pkg = SheetPackage { parts: Vec::new() };
        pkg.set("mimetype", MIMETYPE.as_bytes().to_vec());
        pkg.set("content.xml", content_xml(&snap).into_bytes());
        pkg.set("meta.xml", meta_xml(&snap.properties).into_bytes());
        let has_styles = wb.preserved_part("ods/part/styles.xml").is_some();
        if let Some(styles) = wb.preserved_part("ods/part/styles.xml") {
            pkg.set("styles.xml", styles);
        }
        pkg.set(
            "META-INF/manifest.xml",
            manifest_xml(has_styles).into_bytes(),
        );
        pkg.write_odf()
    }
}

/// Parses the spreadsheet body: `table:table` → `table:table-row` → `table:table-cell` (positional, honouring column/row repeats and formulas).
fn parse_content(xml: &[u8], wb: &mut Workbook) -> Result<(), CodecError> {
    let s = std::str::from_utf8(xml).map_err(|e| CodecError::Parse(e.to_string()))?;
    let mut reader = Reader::from_str(s);

    let mut sheet_id: Option<String> = None;
    let mut row: u32 = 0; // last-used 1-based row index
    let mut cur_row: u32 = 0; // the row the current cells belong to
    let mut row_repeat: u32 = 1;
    let mut col: u32 = 0; // column cursor within the current row

    let mut cur = OdsCell::default();
    let mut in_text_p = false;

    loop {
        match reader
            .read_event()
            .map_err(|e| CodecError::Parse(format!("content xml: {e}")))?
        {
            Event::Start(e) | Event::Empty(e) if local_name(&e) == b"table" => {
                let name = attr_local(&e, b"name").unwrap_or_else(|| "Sheet".to_string());
                sheet_id = Some(wb.add_sheet(&name));
                row = 0;
            }
            Event::Start(e) if local_name(&e) == b"table-row" => {
                row_repeat = attr_local(&e, b"number-rows-repeated")
                    .and_then(|v| v.parse::<u32>().ok())
                    .unwrap_or(1)
                    .max(1);
                cur_row = row.saturating_add(1);
                col = 0;
            }
            Event::End(e) if end_local_name(&e) == b"table-row" => {
                let _ = &e;
                row = row.saturating_add(row_repeat); // repeated (usually empty) rows advance the counter
                row_repeat = 1;
            }
            // A self-closing cell — no `<text:p>` child, flush immediately.
            Event::Empty(e)
                if local_name(&e) == b"table-cell" || local_name(&e) == b"covered-table-cell" =>
            {
                let (cell, repeat) = read_cell_attrs(&e, col.saturating_add(1));
                if let Some(sid) = sheet_id.as_deref() {
                    flush_cell(wb, sid, &cell, cur_row);
                }
                col = col.saturating_add(repeat);
            }
            Event::Start(e)
                if local_name(&e) == b"table-cell" || local_name(&e) == b"covered-table-cell" =>
            {
                let (cell, repeat) = read_cell_attrs(&e, col.saturating_add(1));
                cur = cell;
                cur.repeat = repeat;
                in_text_p = false;
            }
            Event::End(e)
                if end_local_name(&e) == b"table-cell"
                    || end_local_name(&e) == b"covered-table-cell" =>
            {
                let _ = &e;
                if let Some(sid) = sheet_id.as_deref() {
                    flush_cell(wb, sid, &cur, cur_row);
                }
                col = col.saturating_add(cur.repeat);
                cur = OdsCell::default();
            }
            Event::Start(e) if local_name(&e) == b"p" => {
                // `<text:p>` — the cell's display text. Separate paragraphs join with '\n'.
                if !cur.text.is_empty() {
                    cur.text.push('\n');
                }
                in_text_p = true;
            }
            Event::End(e) if end_local_name(&e) == b"p" => {
                let _ = &e;
                in_text_p = false;
            }
            Event::Text(t) => {
                if in_text_p {
                    let txt = t
                        .xml10_content()
                        .map_err(|e| CodecError::Parse(format!("cell text: {e}")))?;
                    cur.text.push_str(&txt);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(())
}

/// The scratch of a parsed ODS cell before it is written into the model.
#[derive(Default)]
struct OdsCell {
    col: u32,
    repeat: u32,
    value_type: String,
    office_value: Option<String>,
    bool_value: Option<String>,
    date_value: Option<String>,
    string_value: Option<String>,
    formula: Option<String>,
    style: Option<String>,
    text: String,
}

fn read_cell_attrs(e: &quick_xml::events::BytesStart, col: u32) -> (OdsCell, u32) {
    let repeat = attr_local(e, b"number-columns-repeated")
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(1)
        .max(1);
    let cell = OdsCell {
        col,
        repeat,
        value_type: attr_local(e, b"value-type").unwrap_or_default(),
        office_value: attr_local(e, b"value"),
        bool_value: attr_local(e, b"boolean-value"),
        date_value: attr_local(e, b"date-value").or_else(|| attr_local(e, b"time-value")),
        string_value: attr_local(e, b"string-value"),
        formula: attr_local(e, b"formula").filter(|f| !f.is_empty()),
        style: attr_local(e, b"style-name").filter(|s| !s.is_empty()),
        text: String::new(),
    };
    (cell, repeat)
}

/// Resolves a parsed ODS cell into a [`CellInput`] and writes it (skipping cells with
/// no value/formula/style); a repeated *populated* cell is placed once at its first column.
fn flush_cell(wb: &mut Workbook, sheet_id: &str, cell: &OdsCell, row: u32) {
    let (cell_type, value) = match cell.value_type.as_str() {
        "float" | "percentage" | "currency" => (
            CellType::Number,
            cell.office_value
                .clone()
                .unwrap_or_else(|| cell.text.clone()),
        ),
        "boolean" => (
            CellType::Bool,
            match cell.bool_value.as_deref() {
                Some(v) if v.eq_ignore_ascii_case("true") || v == "1" => "true".to_string(),
                Some(_) => "false".to_string(),
                None => cell.text.clone(),
            },
        ),
        "date" | "time" => (
            CellType::Text,
            cell.date_value.clone().unwrap_or_else(|| cell.text.clone()),
        ),
        "string" => (
            CellType::Text,
            cell.string_value
                .clone()
                .unwrap_or_else(|| cell.text.clone()),
        ),
        _ => {
            // No declared type: infer from what's present.
            if let Some(v) = &cell.office_value {
                (CellType::Number, v.clone())
            } else if !cell.text.is_empty() {
                (CellType::Text, cell.text.clone())
            } else {
                (CellType::Empty, String::new())
            }
        }
    };
    if value.is_empty() && cell.formula.is_none() && cell.style.is_none() {
        return;
    }
    let cell_type = if value.is_empty() && cell.formula.is_none() {
        CellType::Empty
    } else {
        cell_type
    };
    let reference = crate::sheet::a1::make_ref(cell.col, row);
    wb.set_cell(
        sheet_id,
        &reference,
        CellInput {
            value,
            cell_type,
            formula: cell.formula.clone(),
            style: cell.style.clone(),
        },
    );
}

fn import_meta(xml: &[u8], wb: &mut Workbook) {
    let Ok(s) = std::str::from_utf8(xml) else {
        return;
    };
    let mut reader = Reader::from_str(s);
    let mut cur: Option<&'static str> = None;
    let mut buf = String::new();
    while let Ok(ev) = reader.read_event() {
        match ev {
            Event::Start(e) => {
                cur = match local_name(&e) {
                    b"title" => Some("title"),
                    b"creator" => Some("author"),
                    b"creation-date" => Some("created"),
                    b"date" => Some("modified"),
                    _ => None,
                };
                buf.clear();
            }
            Event::Text(t) => {
                if cur.is_some()
                    && let Ok(txt) = t.xml10_content()
                {
                    buf.push_str(&txt);
                }
            }
            Event::End(_) => {
                if let Some(key) = cur.take()
                    && !buf.is_empty()
                {
                    wb.set_prop(key, &buf);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
}

const CONTENT_NS: &str = r#"xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:of="urn:oasis:names:tc:opendocument:xmlns:of:1.2" xmlns:calcext="urn:org:documentfoundation:names:experimental:calc:xmlns:calcext:1.0""#;

fn content_xml(snap: &WorkbookSnapshot) -> String {
    let mut tables = String::new();
    for sheet in &snap.sheets {
        tables.push_str(&table_xml(sheet));
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content {CONTENT_NS}><office:body><office:spreadsheet>{tables}</office:spreadsheet></office:body></office:document-content>"#
    )
}

fn table_xml(sheet: &SheetView) -> String {
    // Bucket cells by row; compute the extents so gaps collapse into repeats.
    let mut by_row: BTreeMap<u32, Vec<&CellView>> = BTreeMap::new();
    let mut max_col = 1u32;
    for c in &sheet.cells {
        by_row.entry(c.row).or_default().push(c);
        max_col = max_col.max(c.col);
    }
    let mut rows_xml = String::new();
    let mut prev_row = 0u32;
    for (&row, cells) in &by_row {
        if row > prev_row.saturating_add(1) {
            let gap = row.saturating_sub(prev_row).saturating_sub(1);
            let _ = write!(
                rows_xml,
                r#"<table:table-row table:number-rows-repeated="{gap}"><table:table-cell/></table:table-row>"#
            );
        }
        rows_xml.push_str("<table:table-row>");
        let mut prev_col = 0u32;
        let mut ordered = cells.clone();
        ordered.sort_by_key(|c| c.col);
        for c in ordered {
            if c.col > prev_col.saturating_add(1) {
                let gap = c.col.saturating_sub(prev_col).saturating_sub(1);
                let _ = write!(
                    rows_xml,
                    r#"<table:table-cell table:number-columns-repeated="{gap}"/>"#
                );
            }
            rows_xml.push_str(&cell_xml(c));
            prev_col = c.col;
        }
        rows_xml.push_str("</table:table-row>");
        prev_row = row;
    }
    format!(
        r#"<table:table table:name="{}"><table:table-column table:number-columns-repeated="{}"/>{}</table:table>"#,
        escape_attr_quotes(&sheet.name),
        max_col,
        rows_xml
    )
}

fn cell_xml(cell: &CellView) -> String {
    let formula_attr = cell
        .formula
        .as_deref()
        .map(|f| format!(r#" table:formula="{}""#, escape_attr_quotes(f)))
        .unwrap_or_default();
    let style_attr = cell
        .style
        .as_deref()
        .map(|s| format!(r#" table:style-name="{}""#, escape_attr_quotes(s)))
        .unwrap_or_default();
    let (value_attrs, body): (String, String) = match cell.cell_type {
        CellType::Number => (
            format!(
                r#" office:value-type="float" office:value="{}""#,
                escape_attr_quotes(&cell.value)
            ),
            format!("<text:p>{}</text:p>", escape_attr_quotes(&cell.value)),
        ),
        CellType::Text => (
            r#" office:value-type="string""#.to_string(),
            format!("<text:p>{}</text:p>", escape_attr_quotes(&cell.value)),
        ),
        CellType::Bool => {
            let b = if cell.value.eq_ignore_ascii_case("true") || cell.value == "1" {
                "true"
            } else {
                "false"
            };
            (
                format!(r#" office:value-type="boolean" office:boolean-value="{b}""#),
                format!("<text:p>{b}</text:p>"),
            )
        }
        CellType::Error => (
            r#" office:value-type="float" calcext:value-type="error""#.to_string(),
            format!("<text:p>{}</text:p>", escape_attr_quotes(&cell.value)),
        ),
        CellType::Empty => (String::new(), String::new()),
    };
    format!(r"<table:table-cell{value_attrs}{formula_attr}{style_attr}>{body}</table:table-cell>")
}

fn meta_xml(props: &BTreeMap<String, String>) -> String {
    let mut fields = String::new();
    if let Some(v) = props.get("title") {
        let _ = write!(fields, "<dc:title>{}</dc:title>", escape_attr_quotes(v));
    }
    if let Some(v) = props.get("author") {
        let _ = write!(fields, "<dc:creator>{}</dc:creator>", escape_attr_quotes(v));
    }
    if let Some(v) = props.get("created") {
        let _ = write!(
            fields,
            "<meta:creation-date>{}</meta:creation-date>",
            escape_attr_quotes(v)
        );
    }
    if let Some(v) = props.get("modified") {
        let _ = write!(fields, "<dc:date>{}</dc:date>", escape_attr_quotes(v));
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-meta xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:meta="urn:oasis:names:tc:opendocument:xmlns:meta:1.0"><office:meta>{fields}</office:meta></office:document-meta>"#
    )
}

fn manifest_xml(has_styles: bool) -> String {
    let styles_entry = if has_styles {
        r#"<manifest:file-entry manifest:full-path="styles.xml" manifest:media-type="text/xml"/>"#
    } else {
        ""
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0" manifest:version="1.2"><manifest:file-entry manifest:full-path="/" manifest:media-type="{MIMETYPE}"/><manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/><manifest:file-entry manifest:full-path="meta.xml" manifest:media-type="text/xml"/>{styles_entry}</manifest:manifest>"#
    )
}

/// Whether an ODS part is preserved verbatim; the regenerated parts (`mimetype`, `content.xml`, `meta.xml`, the manifest) are not.
fn is_preserved_part(name: &str) -> bool {
    !(name == "mimetype"
        || name == "content.xml"
        || name == "meta.xml"
        || name == "META-INF/manifest.xml"
        || name.ends_with('/'))
}
