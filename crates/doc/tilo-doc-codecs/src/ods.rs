//! `.ods` (`OpenDocument` Spreadsheet) codec. Parses the ODF package's `content.xml`
//! spreadsheet body into the CRDT [`Workbook`] model — sheets, positional cells,
//! cell types, and **formulas** (`table:formula`, preserved verbatim) — and
//! serializes a complete valid package back. Untrusted bytes enter only through
//! the bounded/hardened `SheetPackage` reader (zip-bomb + XXE defence).

use crate::format::{CodecError, Confidence};
use crate::sheet_common::{
    ParsedSheet, ParsedWorkbook, SheetPackage, SheetProvider, attr_local, end_local_name,
    local_name, parse_doc_props,
};
use crate::xml_escape::escape_attr_quotes;
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use std::collections::BTreeMap;
use tilo_doc::sheet::snapshot::CellType;
use tilo_doc::sheet::{CellInput, CellView, SheetView, Workbook, WorkbookSnapshot};
// `write!` into a `String` cannot fail; the discarded `Result` is the
// `fmt::Write` contract, not a swallowed error.
use std::fmt::Write as _;

const MIMETYPE: &str = "application/vnd.oasis.opendocument.spreadsheet";

/// Calc compresses runs of identical cells and rows into one element with a
/// repeat count; expanding a *populated* repeat is capped workbook-wide so a
/// tiny hostile `content.xml` can't claim billions of cells.
const MAX_EXPANDED_REPEATS: usize = 1_000_000;
/// `<text:s text:c="…"/>` space runs stop growing a cell's text past this
/// (a cell holds at most 32767 characters in either office suite), so a few
/// bytes of markup can't claim gigabytes of spaces.
const MAX_SPACE_RUN: usize = 32_767;
/// Excel's grid, which every codec here shares.
const MAX_COL: u32 = 16_384;
const MAX_ROW: u32 = 1_048_576;

pub struct OdsCodec;

impl SheetProvider for OdsCodec {
    fn schema_id(&self) -> &str {
        tilo_doc::sheet::schema::SHEET_SCHEMA_ID
    }

    fn detect(&self, head: &[u8]) -> Confidence {
        if head.starts_with(b"PK") {
            Confidence::Low
        } else {
            Confidence::None
        }
    }

    fn import(&self, bytes: &[u8], wb: &mut Workbook) -> Result<(), CodecError> {
        Self::parse(bytes)?.apply(wb, "ods");
        Ok(())
    }

    fn export(&self, wb: &Workbook) -> Result<Vec<u8>, CodecError> {
        let snap = wb.materialize();
        serialize(&snap, wb.preserved_part("ods/part/styles.xml"))
    }
}

impl OdsCodec {
    /// Parses an untrusted `.ods` package — bounded and XXE-hardened like
    /// [`SheetProvider::import`] — without building a CRDT. Repeated populated
    /// cells and rows are expanded (up to a workbook-wide cap).
    ///
    /// # Errors
    /// [`CodecError::Parse`] for a malformed or over-limit package.
    pub fn parse(bytes: &[u8]) -> Result<ParsedWorkbook, CodecError> {
        let pkg = SheetPackage::read(bytes)?;
        let content = pkg
            .get("content.xml")
            .ok_or_else(|| CodecError::Parse("ods: missing content.xml".into()))?;
        let mut parsed = ParsedWorkbook {
            sheets: parse_content(content)?,
            ..ParsedWorkbook::default()
        };
        if let Some(meta) = pkg.get("meta.xml") {
            parsed.properties = parse_doc_props(
                meta,
                &[
                    (b"title", "title"),
                    (b"creator", "author"),
                    (b"creation-date", "created"),
                    (b"date", "modified"),
                ],
            );
        }
        // Preserve styles + other unmodeled parts so `table:style-name` refs resolve; content.xml is regenerated on export.
        for (name, bytes) in &pkg.parts {
            if is_preserved_part(name) {
                parsed.preserved.insert(name.clone(), bytes.clone());
            }
        }
        Ok(parsed)
    }

    /// Serializes a workbook snapshot (sheets in tab order) as a complete
    /// `.ods` package, without a CRDT. `preserved` holds unmodeled parts by
    /// part name; `styles.xml` (see [`common_styles_part`]) is re-emitted when
    /// present, so `table:style-name` refs can name the styles it declares.
    ///
    /// # Errors
    /// [`CodecError::Export`] if writing the package fails.
    pub fn export_snapshot(
        snap: &WorkbookSnapshot,
        preserved: &BTreeMap<String, Vec<u8>>,
    ) -> Result<Vec<u8>, CodecError> {
        serialize(snap, preserved.get("styles.xml").cloned())
    }
}

fn serialize(snap: &WorkbookSnapshot, styles: Option<Vec<u8>>) -> Result<Vec<u8>, CodecError> {
    let mut pkg = SheetPackage { parts: Vec::new() };
    pkg.set("mimetype", MIMETYPE.as_bytes().to_vec());
    pkg.set("content.xml", content_xml(snap).into_bytes());
    pkg.set("meta.xml", meta_xml(&snap.properties).into_bytes());
    let has_styles = styles.is_some();
    if let Some(styles) = styles {
        pkg.set("styles.xml", styles);
    }
    pkg.set(
        "META-INF/manifest.xml",
        manifest_xml(has_styles).into_bytes(),
    );
    pkg.write_odf()
}

/// A `styles.xml` declaring `office_styles` — `<style:style>` and
/// `<number:*-style>` elements — as common styles, for
/// [`OdsCodec::export_snapshot`]: a cell's style ref can then name any of them.
pub fn common_styles_part(office_styles: &str) -> Vec<u8> {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-styles xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:number="urn:oasis:names:tc:opendocument:xmlns:datastyle:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0" office:version="1.2"><office:styles>{office_styles}</office:styles></office:document-styles>"#
    )
    .into_bytes()
}

/// Parses the spreadsheet body: `table:table` → `table:table-row` →
/// `table:table-cell`, positional, honouring column/row repeats and formulas.
/// A repeated *populated* cell or row is expanded to every position it
/// covers; empty repeats (Calc pads every table to its full width and height
/// that way) only advance the cursor.
#[allow(
    clippy::too_many_lines,
    reason = "one streaming pass keeps every table/row/cell state transition in a single auditable match"
)]
fn parse_content(xml: &[u8]) -> Result<Vec<ParsedSheet>, CodecError> {
    let s = std::str::from_utf8(xml).map_err(|e| CodecError::Parse(e.to_string()))?;
    let mut reader = Reader::from_str(s);

    let mut sheets: Vec<ParsedSheet> = Vec::new();
    let mut row: u32 = 0; // last-used 1-based row index
    let mut row_repeat: u32 = 1;
    let mut col: u32 = 0; // column cursor within the current row
    // The current row's populated cells: (column, repeat, cell).
    let mut row_cells: Vec<(u32, u32, CellInput)> = Vec::new();
    let mut budget = MAX_EXPANDED_REPEATS;

    let mut cur = OdsCell::default();
    let mut in_text_p = false;

    loop {
        match reader
            .read_event()
            .map_err(|e| CodecError::Parse(format!("content xml: {e}")))?
        {
            Event::Start(e) | Event::Empty(e) if local_name(&e) == b"table" => {
                let name = attr_local(&e, b"name").unwrap_or_else(|| "Sheet".to_string());
                sheets.push(ParsedSheet {
                    name,
                    cells: Vec::new(),
                });
                row = 0;
            }
            Event::Start(e) if local_name(&e) == b"table-row" => {
                row_repeat = attr_local(&e, b"number-rows-repeated")
                    .and_then(|v| v.parse::<u32>().ok())
                    .unwrap_or(1)
                    .max(1);
                col = 0;
                row_cells.clear();
            }
            Event::End(e) if end_local_name(&e) == b"table-row" => {
                let _ = &e;
                if let Some(sheet) = sheets.last_mut() {
                    place_row(sheet, &row_cells, row, row_repeat, &mut budget)?;
                }
                row_cells.clear();
                row = row.saturating_add(row_repeat); // repeated (usually empty) rows advance the counter
                row_repeat = 1;
            }
            // A self-closing cell — no `<text:p>` child, flush immediately.
            Event::Empty(e)
                if local_name(&e) == b"table-cell" || local_name(&e) == b"covered-table-cell" =>
            {
                let cell = read_cell_attrs(&e, col.saturating_add(1));
                col = col.saturating_add(cell.repeat);
                if let Some(input) = resolve_cell(&cell) {
                    row_cells.push((cell.col, cell.repeat, input));
                }
            }
            Event::Start(e)
                if local_name(&e) == b"table-cell" || local_name(&e) == b"covered-table-cell" =>
            {
                cur = read_cell_attrs(&e, col.saturating_add(1));
                in_text_p = false;
            }
            Event::End(e)
                if end_local_name(&e) == b"table-cell"
                    || end_local_name(&e) == b"covered-table-cell" =>
            {
                let _ = &e;
                col = col.saturating_add(cur.repeat);
                if let Some(input) = resolve_cell(&cur) {
                    row_cells.push((cur.col, cur.repeat, input));
                }
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
            // ODF spells runs of spaces, tabs and line breaks as elements.
            Event::Empty(e) if in_text_p => match local_name(&e) {
                b"s" => {
                    let count = attr_local(&e, b"c")
                        .and_then(|c| c.parse::<usize>().ok())
                        .unwrap_or(1)
                        .max(1)
                        .min(MAX_SPACE_RUN.saturating_sub(cur.text.len()));
                    cur.text.extend(std::iter::repeat_n(' ', count));
                }
                b"tab" => cur.text.push('\t'),
                b"line-break" => cur.text.push('\n'),
                _ => {}
            },
            Event::Text(t) => {
                if in_text_p {
                    let txt = t
                        .xml10_content()
                        .map_err(|e| CodecError::Parse(format!("cell text: {e}")))?;
                    cur.text.push_str(&txt);
                }
            }
            Event::GeneralRef(r) if in_text_p => {
                cur.text.extend(crate::resolve_general_ref(&r));
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(sheets)
}

/// Writes one parsed row's cells into `sheet` at every row it repeats to and
/// every column each cell repeats to, within the grid, charging each copy
/// past the first against `budget`. A style-only cell (Calc formats whole
/// empty columns that way) is recorded once, at its first position.
fn place_row(
    sheet: &mut ParsedSheet,
    cells: &[(u32, u32, CellInput)],
    last_row: u32,
    row_repeat: u32,
    budget: &mut usize,
) -> Result<(), CodecError> {
    for row_offset in 1..=row_repeat {
        let Some(row) = last_row.checked_add(row_offset).filter(|r| *r <= MAX_ROW) else {
            break;
        };
        for (first_col, repeat, input) in cells {
            let populated = !input.value.is_empty() || input.formula.is_some();
            if !populated && row_offset > 1 {
                continue;
            }
            let repeat = if populated { *repeat } else { 1 };
            for col_offset in 0..repeat {
                let Some(col) = first_col.checked_add(col_offset).filter(|c| *c <= MAX_COL) else {
                    break;
                };
                if row_offset > 1 || col_offset > 0 {
                    *budget = budget.checked_sub(1).ok_or_else(|| {
                        CodecError::Parse(format!(
                            "ods: repeated cells expand past {MAX_EXPANDED_REPEATS} cells"
                        ))
                    })?;
                }
                sheet
                    .cells
                    .push((tilo_doc::sheet::a1::make_ref(col, row), input.clone()));
            }
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

fn read_cell_attrs(e: &quick_xml::events::BytesStart, col: u32) -> OdsCell {
    let repeat = attr_local(e, b"number-columns-repeated")
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(1)
        .max(1);
    OdsCell {
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
    }
}

/// Resolves a parsed ODS cell into a [`CellInput`], or `None` for a cell with no
/// value, formula or style.
fn resolve_cell(cell: &OdsCell) -> Option<CellInput> {
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
        return None;
    }
    let cell_type = if value.is_empty() && cell.formula.is_none() {
        CellType::Empty
    } else {
        cell_type
    };
    Some(CellInput {
        value,
        cell_type,
        formula: cell.formula.clone(),
        style: cell.style.clone(),
    })
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
