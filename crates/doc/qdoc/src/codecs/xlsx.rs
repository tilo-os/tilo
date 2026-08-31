//! `.xlsx` (OOXML `SpreadsheetML`) codec. Parses the OPC package's workbook +
//! worksheet parts into the CRDT [`Workbook`] model — sheets, cells, cell types,
//! and **formulas** (preserved, never flattened to their cached result) — and
//! serializes a complete valid package back. Untrusted bytes enter only through
//! the bounded/hardened [`SheetPackage`] reader (zip-bomb + XXE defence).

use crate::codecs::sheet_common::{
    SheetPackage, SheetProvider, attr_local, end_local_name, local_name, xml_escape,
};
use crate::format::{CodecError, Confidence};
use crate::sheet::snapshot::CellType;
use crate::sheet::{CellInput, Workbook};
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use std::collections::BTreeMap;
// `write!` into a `String` cannot fail; the discarded `Result` is the
// `fmt::Write` contract, not a swallowed error.
use std::fmt::Write as _;

const NS_MAIN: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const NS_REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

pub struct XlsxCodec;

impl SheetProvider for XlsxCodec {
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
        let workbook_xml = pkg
            .get("xl/workbook.xml")
            .ok_or_else(|| CodecError::Parse("xlsx: missing xl/workbook.xml".into()))?;
        let rels = pkg
            .get("xl/_rels/workbook.xml.rels")
            .map(parse_workbook_rels)
            .unwrap_or_default();
        let sst = pkg
            .get("xl/sharedStrings.xml")
            .map(parse_shared_strings)
            .unwrap_or_default();
        let sheet_defs = parse_workbook_sheets(workbook_xml)?;

        for (name, rid) in sheet_defs {
            let sheet_id = wb.add_sheet(&name);
            let Some(target) = rels.get(&rid) else {
                continue;
            };
            let part = normalize_xl_target(target);
            let Some(ws_xml) = pkg.get(&part) else {
                continue;
            };
            parse_worksheet(ws_xml, &sst, wb, &sheet_id)?;
        }

        if let Some(core) = pkg.get("docProps/core.xml") {
            import_core_props(core, wb);
        }
        // Preserve styles + theme so cell `s=` refs resolve and unmodeled parts survive; regenerated structural parts are not.
        for name in pkg.names() {
            if is_preserved_part(name)
                && let Some(b) = pkg.get(name)
            {
                wb.set_preserved(&format!("xlsx/part/{name}"), b);
            }
        }
        Ok(())
    }

    fn export(&self, wb: &Workbook) -> Result<Vec<u8>, CodecError> {
        let snap = wb.materialize();
        let mut pkg = SheetPackage { parts: Vec::new() };

        let styles_xml = wb.preserved_part("xlsx/part/xl/styles.xml");
        let has_styles = styles_xml.is_some();

        pkg.set(
            "[Content_Types].xml",
            content_types_xml(snap.sheets.len(), has_styles).into_bytes(),
        );
        pkg.set("_rels/.rels", root_rels_xml().into_bytes());
        pkg.set("xl/workbook.xml", workbook_xml(&snap).into_bytes());
        pkg.set(
            "xl/_rels/workbook.xml.rels",
            workbook_rels_xml(snap.sheets.len(), has_styles).into_bytes(),
        );
        for (i, sheet) in snap.sheets.iter().enumerate() {
            pkg.set(
                &format!("xl/worksheets/sheet{}.xml", i.saturating_add(1)),
                worksheet_xml(sheet).into_bytes(),
            );
        }
        // styles.xml: re-emit the preserved original, else a minimal default.
        pkg.set(
            "xl/styles.xml",
            styles_xml.unwrap_or_else(|| minimal_styles_xml().into_bytes()),
        );
        pkg.set(
            "docProps/core.xml",
            core_props_xml(&snap.properties).into_bytes(),
        );

        pkg.write_opc()
    }
}

/// `(sheet name, r:id)` in document order.
fn parse_workbook_sheets(xml: &[u8]) -> Result<Vec<(String, String)>, CodecError> {
    let s = std::str::from_utf8(xml).map_err(|e| CodecError::Parse(e.to_string()))?;
    let mut reader = Reader::from_str(s);
    let mut out = Vec::new();
    loop {
        match reader
            .read_event()
            .map_err(|e| CodecError::Parse(format!("workbook xml: {e}")))?
        {
            Event::Empty(e) | Event::Start(e) if local_name(&e) == b"sheet" => {
                let name = attr_local(&e, b"name").unwrap_or_default();
                let rid = attr_local(&e, b"id").unwrap_or_default(); // r:id
                out.push((name, rid));
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

/// `rId → target path` from xl/_rels/workbook.xml.rels.
fn parse_workbook_rels(xml: &[u8]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Ok(s) = std::str::from_utf8(xml) else {
        return out;
    };
    let mut reader = Reader::from_str(s);
    while let Ok(ev) = reader.read_event() {
        match ev {
            Event::Empty(e) | Event::Start(e) if local_name(&e) == b"Relationship" => {
                if let (Some(id), Some(target)) = (attr_local(&e, b"Id"), attr_local(&e, b"Target"))
                {
                    out.insert(id, target);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

/// A worksheet rels `Target` (e.g. `worksheets/sheet1.xml`, `/xl/worksheets/…`) resolved to a package part path.
fn normalize_xl_target(target: &str) -> String {
    let t = target.trim_start_matches('/');
    if t.starts_with("xl/") {
        t.to_string()
    } else {
        format!("xl/{t}")
    }
}

/// The shared-string table: each `<si>` → its concatenated `<t>` text.
fn parse_shared_strings(xml: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(s) = std::str::from_utf8(xml) else {
        return out;
    };
    let mut reader = Reader::from_str(s);
    let mut in_si = false;
    let mut in_t = false;
    let mut cur = String::new();
    while let Ok(ev) = reader.read_event() {
        match ev {
            Event::Start(e) => match local_name(&e) {
                b"si" => {
                    in_si = true;
                    cur.clear();
                }
                b"t" if in_si => in_t = true,
                _ => {}
            },
            Event::End(e) => match end_local_name(&e) {
                b"si" => {
                    in_si = false;
                    out.push(std::mem::take(&mut cur));
                }
                b"t" => in_t = false,
                _ => {}
            },
            Event::Text(t) if in_t => {
                if let Ok(txt) = t.xml10_content() {
                    cur.push_str(&txt);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

/// Parses a worksheet's `<sheetData>` into the workbook model, resolving shared
/// strings and preserving formulas: `<f>` is the formula source, `<v>` the value
/// (or a shared-string index when `t="s"`), `<is><t>` an inline string.
fn parse_worksheet(
    xml: &[u8],
    sst: &[String],
    wb: &mut Workbook,
    sheet_id: &str,
) -> Result<(), CodecError> {
    let s = std::str::from_utf8(xml).map_err(|e| CodecError::Parse(e.to_string()))?;
    let mut reader = Reader::from_str(s);

    let mut cur_ref: Option<String> = None;
    let mut cur_t = String::new();
    let mut cur_s: Option<String> = None;
    let mut in_f = false;
    let mut in_v = false;
    let mut in_is_t = false; // inside <is>…<t>
    let mut formula = String::new();
    let mut value = String::new();
    let mut has_formula = false;

    loop {
        match reader
            .read_event()
            .map_err(|e| CodecError::Parse(format!("worksheet xml: {e}")))?
        {
            // A self-closing `<c .../>` — an empty (possibly styled) cell.
            Event::Empty(e) if local_name(&e) == b"c" => {
                flush_cell(
                    wb,
                    sheet_id,
                    sst,
                    attr_local(&e, b"r"),
                    &attr_local(&e, b"t").unwrap_or_default(),
                    attr_local(&e, b"s"),
                    "",
                    "",
                    false,
                );
            }
            Event::Start(e) if local_name(&e) == b"c" => {
                cur_ref = attr_local(&e, b"r");
                cur_t = attr_local(&e, b"t").unwrap_or_default();
                cur_s = attr_local(&e, b"s");
                formula.clear();
                value.clear();
                has_formula = false;
                in_f = false;
                in_v = false;
                in_is_t = false;
            }
            Event::Start(e) => match local_name(&e) {
                b"f" => {
                    in_f = true;
                    has_formula = true;
                }
                b"v" => in_v = true,
                b"t" => in_is_t = true, // inside <c><is><t>
                _ => {}
            },
            Event::End(e) => match end_local_name(&e) {
                b"f" => in_f = false,
                b"v" => in_v = false,
                b"t" => in_is_t = false,
                b"c" => {
                    flush_cell(
                        wb,
                        sheet_id,
                        sst,
                        cur_ref.take(),
                        &cur_t,
                        cur_s.take(),
                        &value,
                        &formula,
                        has_formula,
                    );
                }
                _ => {}
            },
            Event::Text(t) => {
                let txt = t
                    .xml10_content()
                    .map_err(|e| CodecError::Parse(format!("cell text: {e}")))?;
                if in_f {
                    formula.push_str(&txt);
                } else if in_v || in_is_t {
                    value.push_str(&txt);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(())
}

/// Resolves one parsed `<c>` cell into a [`CellInput`] and writes it (skipped if it
/// carries no value/formula/style — the grid is sparse); `t="s"` resolves against `sst`.
#[allow(clippy::too_many_arguments)]
fn flush_cell(
    wb: &mut Workbook,
    sheet_id: &str,
    sst: &[String],
    reference: Option<String>,
    t: &str,
    style: Option<String>,
    value: &str,
    formula: &str,
    has_formula: bool,
) {
    let Some(reference) = reference else { return };
    let (mut cell_type, resolved) = match t {
        "s" => {
            let idx: usize = value.trim().parse().unwrap_or(usize::MAX);
            (CellType::Text, sst.get(idx).cloned().unwrap_or_default())
        }
        // "d" is an ISO date string, preserved as text like "str"/"inlineStr".
        "str" | "inlineStr" | "d" => (CellType::Text, value.to_string()),
        "b" => (
            CellType::Bool,
            if value.trim() == "1" || value.eq_ignore_ascii_case("true") {
                "true".to_string()
            } else {
                "false".to_string()
            },
        ),
        "e" => (CellType::Error, value.to_string()),
        _ => (CellType::Number, value.to_string()), // absent or "n"
    };
    if resolved.is_empty() && !has_formula && style.is_none() {
        return;
    }
    // A style-only (or truly blank) cell records as Empty.
    if resolved.is_empty() && !has_formula {
        cell_type = CellType::Empty;
    }
    let input = CellInput {
        value: resolved,
        cell_type,
        formula: has_formula.then(|| formula.to_string()),
        style,
    };
    wb.set_cell(sheet_id, &reference, input);
}

fn import_core_props(xml: &[u8], wb: &mut Workbook) {
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
                    b"created" => Some("created"),
                    b"modified" => Some("modified"),
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

fn content_types_xml(nsheets: usize, has_styles: bool) -> String {
    let mut overrides = String::new();
    overrides.push_str(r#"<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>"#);
    for i in 0..nsheets {
        let _ = write!(
            overrides,
            r#"<Override PartName="/xl/worksheets/sheet{}.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>"#,
            i.saturating_add(1)
        );
    }
    if has_styles {
        overrides.push_str(r#"<Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/>"#);
    }
    overrides.push_str(r#"<Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/>"#);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/>{overrides}</Types>"#
    )
}

fn root_rels_xml() -> String {
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/><Relationship Id="rIdCore" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/></Relationships>"#.to_string()
}

fn workbook_xml(snap: &crate::sheet::WorkbookSnapshot) -> String {
    let mut sheets = String::new();
    for (i, sheet) in snap.sheets.iter().enumerate() {
        let n = i.saturating_add(1);
        let _ = write!(
            sheets,
            r#"<sheet name="{}" sheetId="{n}" r:id="rIdSheet{n}"/>"#,
            xml_escape(&sheet.name),
        );
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="{NS_MAIN}" xmlns:r="{NS_REL}"><sheets>{sheets}</sheets></workbook>"#
    )
}

fn workbook_rels_xml(nsheets: usize, has_styles: bool) -> String {
    let mut rels = String::new();
    for i in 0..nsheets {
        let n = i.saturating_add(1);
        let _ = write!(
            rels,
            r#"<Relationship Id="rIdSheet{n}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet{n}.xml"/>"#,
        );
    }
    if has_styles {
        rels.push_str(r#"<Relationship Id="rIdStyles" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>"#);
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{rels}</Relationships>"#
    )
}

fn worksheet_xml(sheet: &crate::sheet::SheetView) -> String {
    // Group cells by row (cells are already row-major sorted).
    let mut rows: BTreeMap<u32, Vec<&crate::sheet::CellView>> = BTreeMap::new();
    for cell in &sheet.cells {
        rows.entry(cell.row).or_default().push(cell);
    }
    let mut data = String::new();
    for (row, cells) in &rows {
        let _ = write!(data, r#"<row r="{row}">"#);
        for cell in cells {
            data.push_str(&cell_xml(cell));
        }
        data.push_str("</row>");
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="{NS_MAIN}" xmlns:r="{NS_REL}"><sheetData>{data}</sheetData></worksheet>"#
    )
}

fn bool01(value: &str) -> &'static str {
    if value.eq_ignore_ascii_case("true") || value == "1" {
        "1"
    } else {
        "0"
    }
}

fn cell_xml(cell: &crate::sheet::CellView) -> String {
    let s_attr = cell
        .style
        .as_deref()
        .map(|s| format!(r#" s="{}""#, xml_escape(s)))
        .unwrap_or_default();
    let f_xml = cell
        .formula
        .as_deref()
        .map(|f| format!("<f>{}</f>", xml_escape(f)))
        .unwrap_or_default();
    let has_formula = cell.formula.is_some();

    // Value serialization + the `t` attribute per cell type: a formula's string result
    // uses `t="str"` (a cached formula string), a literal string uses `t="inlineStr"`.
    let (t_attr, body): (&str, String) = match cell.cell_type {
        CellType::Number => ("", format!("{f_xml}<v>{}</v>", xml_escape(&cell.value))),
        CellType::Bool => (" t=\"b\"", format!("{f_xml}<v>{}</v>", bool01(&cell.value))),
        CellType::Error => (
            " t=\"e\"",
            format!("{f_xml}<v>{}</v>", xml_escape(&cell.value)),
        ),
        CellType::Text if has_formula => (
            " t=\"str\"",
            format!("{f_xml}<v>{}</v>", xml_escape(&cell.value)),
        ),
        CellType::Text => (
            " t=\"inlineStr\"",
            format!(
                r#"<is><t xml:space="preserve">{}</t></is>"#,
                xml_escape(&cell.value)
            ),
        ),
        CellType::Empty => ("", f_xml),
    };
    format!(r#"<c r="{}"{t_attr}{s_attr}>{body}</c>"#, cell.reference)
}

fn minimal_styles_xml() -> String {
    // A minimal but valid styles part (a single default cell format at index 0).
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><fonts count="1"><font><sz val="11"/><name val="Calibri"/></font></fonts><fills count="1"><fill><patternFill patternType="none"/></fill></fills><borders count="1"><border/></borders><cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs><cellXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/></cellXfs></styleSheet>"#.to_string()
}

fn core_props_xml(props: &BTreeMap<String, String>) -> String {
    let mut fields = String::new();
    if let Some(v) = props.get("title") {
        let _ = write!(fields, "<dc:title>{}</dc:title>", xml_escape(v));
    }
    if let Some(v) = props.get("author") {
        let _ = write!(fields, "<dc:creator>{}</dc:creator>", xml_escape(v));
    }
    if let Some(v) = props.get("created") {
        let _ = write!(
            fields,
            r#"<dcterms:created xsi:type="dcterms:W3CDTF">{}</dcterms:created>"#,
            xml_escape(v)
        );
    }
    if let Some(v) = props.get("modified") {
        let _ = write!(
            fields,
            r#"<dcterms:modified xsi:type="dcterms:W3CDTF">{}</dcterms:modified>"#,
            xml_escape(v)
        );
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">{fields}</cp:coreProperties>"#
    )
}

/// Whether a part should be preserved verbatim; the regenerated structural parts (workbook, worksheets, rels, content-types, sharedStrings, docProps) are not.
fn is_preserved_part(name: &str) -> bool {
    let structural = name == "[Content_Types].xml"
        || name == "_rels/.rels"
        || name == "xl/workbook.xml"
        || name == "xl/_rels/workbook.xml.rels"
        || name == "xl/sharedStrings.xml"
        || name.starts_with("xl/worksheets/")
        || name.starts_with("docProps/")
        || name.ends_with('/');
    !structural
}
