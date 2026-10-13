//! `.xlsx` (OOXML `SpreadsheetML`) codec. Parses the OPC package's workbook +
//! worksheet parts into the CRDT [`Workbook`] model — sheets, cells, cell types,
//! and **formulas** (preserved, never flattened to their cached result) — and
//! serializes a complete valid package back. Untrusted bytes enter only through
//! the bounded/hardened `ZipPackage` reader (zip-bomb + XXE defence).

use crate::format::CodecError;
use crate::sheet_common::{
    ParsedSheet, ParsedWorkbook, SheetProvider, attr_local, end_local_name, local_name,
};
use crate::xml_escape::escape_attr_quotes;
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use tilo_doc::sheet::a1::{parse_ref, shift_formula};
use tilo_doc::sheet::snapshot::CellType;
use tilo_doc::sheet::{CellInput, Workbook, WorkbookSnapshot};
// `write!` into a `String` cannot fail; the discarded `Result` is the
// `fmt::Write` contract, not a swallowed error.
use std::fmt::Write as _;

const NS_MAIN: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const NS_REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

pub struct XlsxCodec;

impl SheetProvider for XlsxCodec {
    fn import(&self, bytes: &[u8], wb: &mut Workbook) -> Result<(), CodecError> {
        Self::parse(bytes)?.apply(wb, "xlsx");
        Ok(())
    }

    fn export(&self, wb: &Workbook) -> Result<Vec<u8>, CodecError> {
        let snap = wb.materialize();
        let preserved = |name: &str| wb.preserved_part(&format!("xlsx/part/{name}"));
        serialize(&snap, &preserved)
    }
}

impl XlsxCodec {
    /// Parses an untrusted `.xlsx` package — bounded and XXE-hardened like
    /// [`SheetProvider::import`] — without building a CRDT. Shared formulas
    /// are expanded to every cell of their range.
    ///
    /// # Errors
    /// [`CodecError::Parse`] for a malformed or over-limit package.
    pub fn parse(bytes: &[u8]) -> Result<ParsedWorkbook, CodecError> {
        let pkg = crate::package::ZipPackage::read(bytes)?;
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
        let mut parsed = ParsedWorkbook::default();
        for (name, rid) in parse_workbook_sheets(workbook_xml)? {
            let mut sheet = ParsedSheet {
                name,
                cells: Vec::new(),
            };
            if let Some(ws_xml) = rels
                .get(&rid)
                .and_then(|target| pkg.get(&normalize_xl_target(target)))
            {
                sheet.cells = parse_worksheet(ws_xml, &sst)?;
            }
            parsed.sheets.push(sheet);
        }
        if let Some(core) = pkg.get("docProps/core.xml") {
            parsed.properties = crate::office_props::element_props(
                core,
                crate::office_props::ooxml::core_prop_for_element,
            );
        }
        // Preserve styles + theme so cell `s=` refs resolve and unmodeled parts survive; regenerated structural parts are not.
        for (name, bytes) in &pkg.parts {
            if is_preserved_part(name) {
                parsed.preserved.insert(name.clone(), bytes.clone());
            }
        }
        Ok(parsed)
    }

    /// Serializes a workbook snapshot (sheets in tab order) as a complete
    /// `.xlsx` package, without a CRDT. `preserved` holds unmodeled parts by
    /// part name; `xl/styles.xml` (see [`number_format_styles`]) and
    /// `xl/theme/theme1.xml` are re-emitted when present. Tab names Excel
    /// would refuse are made valid and unique.
    ///
    /// # Errors
    /// [`CodecError::Export`] if writing the package fails.
    pub fn export_snapshot(
        snap: &WorkbookSnapshot,
        preserved: &BTreeMap<String, Vec<u8>>,
    ) -> Result<Vec<u8>, CodecError> {
        serialize(snap, &|name: &str| preserved.get(name).cloned())
    }
}

fn serialize(
    snap: &WorkbookSnapshot,
    preserved: &dyn Fn(&str) -> Option<Vec<u8>>,
) -> Result<Vec<u8>, CodecError> {
    let mut pkg = crate::package::ZipPackage { parts: Vec::new() };

    let styles_xml = preserved("xl/styles.xml");
    // Styles routinely refer to `theme/theme1.xml` (notably for font and
    // fill colours). Keeping the theme part *and its OPC relationship*
    // avoids turning a styled workbook into Office's default palette on a
    // content edit.
    let theme_xml = preserved("xl/theme/theme1.xml");
    let has_theme = theme_xml.is_some();

    pkg.set(
        "[Content_Types].xml",
        content_types_xml(snap.sheets.len(), has_theme).into_bytes(),
    );
    pkg.set("_rels/.rels", root_rels_xml().into_bytes());
    pkg.set("xl/workbook.xml", workbook_xml(snap).into_bytes());
    pkg.set(
        "xl/_rels/workbook.xml.rels",
        workbook_rels_xml(snap.sheets.len(), has_theme).into_bytes(),
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
    if let Some(theme) = theme_xml {
        pkg.set("xl/theme/theme1.xml", theme);
    }
    pkg.set(
        "docProps/core.xml",
        core_props_xml(&snap.properties).into_bytes(),
    );

    pkg.write_opc()
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
            Event::GeneralRef(r) if in_t => {
                cur.extend(crate::resolve_general_ref(&r));
            }
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

/// Parses a worksheet's `<sheetData>` into cells, resolving shared strings and
/// preserving formulas: `<f>` is the formula source, `<v>` the value (or a
/// shared-string index when `t="s"`), `<is><t>` an inline string. A shared
/// formula (`<f t="shared" si="…">`) is written once on its anchor cell; the
/// other cells of its range carry only the index and get the anchor's formula
/// with its relative references shifted, as Excel itself reads them.
#[allow(
    clippy::too_many_lines,
    reason = "one streaming pass keeps every <c>/<f>/<v> state transition in a single auditable match"
)]
fn parse_worksheet(xml: &[u8], sst: &[String]) -> Result<Vec<(String, CellInput)>, CodecError> {
    let s = std::str::from_utf8(xml).map_err(|e| CodecError::Parse(e.to_string()))?;
    let mut reader = Reader::from_str(s);
    let mut cells = Vec::new();

    let mut cur_ref: Option<String> = None;
    let mut cur_t = String::new();
    let mut cur_s: Option<String> = None;
    let mut in_f = false;
    let mut in_v = false;
    let mut in_is_t = false; // inside <is>…<t>
    let mut formula = String::new();
    let mut value = String::new();
    let mut has_formula = false;
    // The shared-formula index of the `<f>` being read, and every anchor seen
    // so far: index → (template, anchor column, anchor row).
    let mut shared_index: Option<String> = None;
    let mut shared: HashMap<String, (String, u32, u32)> = HashMap::new();

    loop {
        match reader
            .read_event()
            .map_err(|e| CodecError::Parse(format!("worksheet xml: {e}")))?
        {
            // A self-closing `<c .../>` — an empty (possibly styled) cell.
            Event::Empty(e) if local_name(&e) == b"c" => {
                push_cell(
                    &mut cells,
                    sst,
                    attr_local(&e, b"r"),
                    &attr_local(&e, b"t").unwrap_or_default(),
                    attr_local(&e, b"s"),
                    "",
                    None,
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
                shared_index = None;
            }
            // `<f t="shared" si="0"/>` — a cell inside a shared formula's range.
            Event::Empty(e) if local_name(&e) == b"f" => {
                has_formula = shared_formula_index(&e)
                    .and_then(|si| expand_shared(&shared, &si, cur_ref.as_deref()))
                    .map(|expanded| formula = expanded)
                    .is_some();
            }
            Event::Start(e) => match local_name(&e) {
                b"f" => {
                    in_f = true;
                    has_formula = true;
                    shared_index = shared_formula_index(&e);
                }
                b"v" => in_v = true,
                b"t" => in_is_t = true, // inside <c><is><t>
                _ => {}
            },
            Event::End(e) => match end_local_name(&e) {
                b"f" => {
                    in_f = false;
                    if let Some(si) = shared_index.take() {
                        if formula.is_empty() {
                            match expand_shared(&shared, &si, cur_ref.as_deref()) {
                                Some(expanded) => formula = expanded,
                                None => has_formula = false,
                            }
                        } else if let Some((col, row)) = cur_ref.as_deref().and_then(parse_ref) {
                            shared.insert(si, (formula.clone(), col, row));
                        }
                    }
                }
                b"v" => in_v = false,
                b"t" => in_is_t = false,
                b"c" => {
                    push_cell(
                        &mut cells,
                        sst,
                        cur_ref.take(),
                        &cur_t,
                        cur_s.take(),
                        &value,
                        has_formula.then_some(formula.as_str()),
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
            Event::GeneralRef(r) => {
                let resolved = crate::resolve_general_ref(&r);
                if in_f {
                    formula.extend(resolved);
                } else if in_v || in_is_t {
                    value.extend(resolved);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(cells)
}

/// The `si` of a `<f t="shared">`, or `None` for an ordinary formula.
fn shared_formula_index(e: &quick_xml::events::BytesStart) -> Option<String> {
    (attr_local(e, b"t").as_deref() == Some("shared"))
        .then(|| attr_local(e, b"si"))
        .flatten()
}

/// The formula a shared-formula cell at `reference` reads: its anchor's
/// template shifted by the distance from the anchor. `None` when the anchor
/// hasn't been seen (a malformed sheet), so the cell keeps just its value.
fn expand_shared(
    shared: &HashMap<String, (String, u32, u32)>,
    si: &str,
    reference: Option<&str>,
) -> Option<String> {
    let (template, anchor_col, anchor_row) = shared.get(si)?;
    let (col, row) = parse_ref(reference?)?;
    Some(shift_formula(
        template,
        i64::from(col).saturating_sub(i64::from(*anchor_col)),
        i64::from(row).saturating_sub(i64::from(*anchor_row)),
    ))
}

/// Resolves one parsed `<c>` cell into a [`CellInput`] (skipped if it carries no
/// value/formula/style — the grid is sparse — or has no valid reference);
/// `t="s"` resolves against `sst`.
fn push_cell(
    cells: &mut Vec<(String, CellInput)>,
    sst: &[String],
    reference: Option<String>,
    t: &str,
    style: Option<String>,
    value: &str,
    formula: Option<&str>,
) {
    let Some(reference) = reference.filter(|r| parse_ref(r).is_some()) else {
        return;
    };
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
    if resolved.is_empty() && formula.is_none() && style.is_none() {
        return;
    }
    // A style-only (or truly blank) cell records as Empty.
    if resolved.is_empty() && formula.is_none() {
        cell_type = CellType::Empty;
    }
    cells.push((
        reference,
        CellInput {
            value: resolved,
            cell_type,
            formula: formula.map(str::to_string),
            style,
        },
    ));
}

fn content_types_xml(nsheets: usize, has_theme: bool) -> String {
    let mut overrides = String::new();
    overrides.push_str(r#"<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>"#);
    for i in 0..nsheets {
        let _ = write!(
            overrides,
            r#"<Override PartName="/xl/worksheets/sheet{}.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>"#,
            i.saturating_add(1)
        );
    }
    // `styles.xml` is always emitted (including the minimal default), so it
    // must always be declared and related. Omitting it made style IDs appear
    // invalid to strict consumers when an imported file lacked that part.
    overrides.push_str(r#"<Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/>"#);
    if has_theme {
        overrides.push_str(r#"<Override PartName="/xl/theme/theme1.xml" ContentType="application/vnd.openxmlformats-officedocument.theme+xml"/>"#);
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

fn workbook_xml(snap: &WorkbookSnapshot) -> String {
    let mut sheets = String::new();
    let mut used = HashSet::new();
    for (i, sheet) in snap.sheets.iter().enumerate() {
        let n = i.saturating_add(1);
        let _ = write!(
            sheets,
            r#"<sheet name="{}" sheetId="{n}" r:id="rIdSheet{n}"/>"#,
            escape_attr_quotes(&unique_sheet_name(&sheet.name, &mut used)),
        );
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="{NS_MAIN}" xmlns:r="{NS_REL}"><sheets>{sheets}</sheets></workbook>"#
    )
}

/// Excel refuses a tab name that is empty, longer than 31 characters or
/// contains any of `[]:*?/\`.
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

/// A valid, workbook-unique Excel tab name. Excel compares tab names
/// case-insensitively, and two names can also collapse to the same value
/// once forbidden characters are replaced or the length limit applies; export
/// must not fail — or drop a tab — because of that collision.
fn unique_sheet_name(requested: &str, used: &mut HashSet<String>) -> String {
    let base = sanitize_sheet_name(requested);
    if used.insert(base.to_lowercase()) {
        return base;
    }
    let mut n = 2usize;
    loop {
        let suffix = format!(" ({n})");
        let keep = 31usize.saturating_sub(suffix.chars().count());
        let stem: String = base.chars().take(keep).collect();
        let candidate = format!("{stem}{suffix}");
        if used.insert(candidate.to_lowercase()) {
            return candidate;
        }
        n = n.saturating_add(1);
    }
}

/// The cell style refs (`s=` values) in a `styles.xml` whose number format
/// shows a date, time or duration — Excel stores those as plain serial
/// numbers, so the format is the only sign. Built-in formats 14–22 and 45–47
/// count, as does any custom code whose first section uses a date/time token
/// outside quotes, brackets and escapes.
pub fn date_style_refs(styles_xml: &[u8]) -> BTreeSet<String> {
    let mut refs = BTreeSet::new();
    let Ok(s) = std::str::from_utf8(styles_xml) else {
        return refs;
    };
    let mut reader = Reader::from_str(s);
    let mut custom: HashMap<String, String> = HashMap::new();
    let mut in_cell_xfs = false;
    let mut index = 0usize;
    while let Ok(ev) = reader.read_event() {
        match ev {
            Event::Start(e) | Event::Empty(e) if local_name(&e) == b"numFmt" => {
                if let (Some(id), Some(code)) =
                    (attr_local(&e, b"numFmtId"), attr_local(&e, b"formatCode"))
                {
                    custom.insert(id, code);
                }
            }
            Event::Start(e) if local_name(&e) == b"cellXfs" => in_cell_xfs = true,
            Event::End(e) if end_local_name(&e) == b"cellXfs" => in_cell_xfs = false,
            Event::Start(e) | Event::Empty(e) if in_cell_xfs && local_name(&e) == b"xf" => {
                let is_date = attr_local(&e, b"numFmtId").is_some_and(|id| match custom.get(&id) {
                    Some(code) => is_date_format_code(code),
                    None => matches!(id.parse::<u32>(), Ok(14..=22 | 45..=47)),
                });
                if is_date {
                    refs.insert(index.to_string());
                }
                index = index.saturating_add(1);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    refs
}

/// Whether a custom number-format code displays a date, time or duration:
/// a `d`/`m`/`y`/`h`/`s` token (or an `AM/PM` marker) in its first section,
/// ignoring quoted text, `[...]` colour/locale brackets and escaped
/// characters — except an `[h]`/`[mm]`/`[ss]` elapsed-time bracket.
fn is_date_format_code(code: &str) -> bool {
    let mut chars = code.chars();
    while let Some(c) = chars.next() {
        match c {
            ';' => return false,
            '\\' | '_' | '*' => {
                chars.next();
            }
            '"' => {
                for q in chars.by_ref() {
                    if q == '"' {
                        break;
                    }
                }
            }
            '[' => {
                let mut inner = String::new();
                for b in chars.by_ref() {
                    if b == ']' {
                        break;
                    }
                    inner.push(b);
                }
                let lower = inner.to_ascii_lowercase();
                if !lower.is_empty() && lower.chars().all(|l| matches!(l, 'h' | 'm' | 's')) {
                    return true;
                }
            }
            'd' | 'D' | 'm' | 'M' | 'y' | 'Y' | 'h' | 'H' | 's' | 'S' => return true,
            'a' | 'A' => {
                let rest: String = chars.clone().take(4).collect();
                if rest.eq_ignore_ascii_case("m/pm")
                    || rest.get(..2).is_some_and(|r| r.eq_ignore_ascii_case("/p"))
                {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// A `styles.xml` whose cell style `i + 1` applies number-format code
/// `codes[i]` (style `0` stays the default), for [`XlsxCodec::export_snapshot`]:
/// set a cell's style to the index as a string.
pub fn number_format_styles(codes: &[&str]) -> Vec<u8> {
    let styles: Vec<CellStyle<'_>> = codes
        .iter()
        .map(|code| CellStyle {
            number_format: Some(code),
            bold: false,
        })
        .collect();
    cell_styles(&styles)
}

/// One cell style for [`cell_styles`]: a number-format code, bold text, or both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellStyle<'a> {
    pub number_format: Option<&'a str>,
    pub bold: bool,
}

/// A `styles.xml` whose cell style `i + 1` applies `styles[i]` (style `0`
/// stays the default); set a cell's style to the index as a string.
pub fn cell_styles(styles: &[CellStyle<'_>]) -> Vec<u8> {
    let mut codes: Vec<&str> = Vec::new();
    let mut num_fmts = String::new();
    let mut xfs = String::from(r#"<xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/>"#);
    for style in styles {
        let num_fmt = style.number_format.map_or(0, |code| {
            let index = codes.iter().position(|c| *c == code).unwrap_or_else(|| {
                codes.push(code);
                let _ = write!(
                    num_fmts,
                    r#"<numFmt numFmtId="{}" formatCode="{}"/>"#,
                    codes.len().saturating_add(163),
                    escape_attr_quotes(code)
                );
                codes.len().saturating_sub(1)
            });
            index.saturating_add(164)
        });
        let font = u8::from(style.bold);
        let apply_num = if num_fmt == 0 {
            ""
        } else {
            r#" applyNumberFormat="1""#
        };
        let apply_font = if style.bold { r#" applyFont="1""# } else { "" };
        let _ = write!(
            xfs,
            r#"<xf numFmtId="{num_fmt}" fontId="{font}" fillId="0" borderId="0" xfId="0"{apply_num}{apply_font}/>"#
        );
    }
    let fonts = if styles.iter().any(|style| style.bold) {
        r#"<fonts count="2"><font><sz val="11"/><name val="Calibri"/></font><font><b/><sz val="11"/><name val="Calibri"/></font></fonts>"#
    } else {
        r#"<fonts count="1"><font><sz val="11"/><name val="Calibri"/></font></fonts>"#
    };
    let count = codes.len();
    let xf_count = styles.len().saturating_add(1);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="{NS_MAIN}"><numFmts count="{count}">{num_fmts}</numFmts>{fonts}<fills count="1"><fill><patternFill patternType="none"/></fill></fills><borders count="1"><border/></borders><cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs><cellXfs count="{xf_count}">{xfs}</cellXfs></styleSheet>"#
    )
    .into_bytes()
}

fn workbook_rels_xml(nsheets: usize, has_theme: bool) -> String {
    let mut rels = String::new();
    for i in 0..nsheets {
        let n = i.saturating_add(1);
        let _ = write!(
            rels,
            r#"<Relationship Id="rIdSheet{n}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet{n}.xml"/>"#,
        );
    }
    rels.push_str(r#"<Relationship Id="rIdStyles" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>"#);
    if has_theme {
        rels.push_str(r#"<Relationship Id="rIdTheme" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme" Target="theme/theme1.xml"/>"#);
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{rels}</Relationships>"#
    )
}

fn worksheet_xml(sheet: &tilo_doc::sheet::SheetView) -> String {
    // Group cells by row (cells are already row-major sorted).
    let mut rows: BTreeMap<u32, Vec<&tilo_doc::sheet::CellView>> = BTreeMap::new();
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

fn cell_xml(cell: &tilo_doc::sheet::CellView) -> String {
    let s_attr = cell
        .style
        .as_deref()
        .map(|s| format!(r#" s="{}""#, escape_attr_quotes(s)))
        .unwrap_or_default();
    let f_xml = cell
        .formula
        .as_deref()
        .map(|f| format!("<f>{}</f>", escape_attr_quotes(f)))
        .unwrap_or_default();
    let has_formula = cell.formula.is_some();

    // Value serialization + the `t` attribute per cell type: a formula's string result
    // uses `t="str"` (a cached formula string), a literal string uses `t="inlineStr"`.
    let (t_attr, body): (&str, String) = match cell.cell_type {
        CellType::Number => (
            "",
            format!("{f_xml}<v>{}</v>", escape_attr_quotes(&cell.value)),
        ),
        CellType::Bool => (" t=\"b\"", format!("{f_xml}<v>{}</v>", bool01(&cell.value))),
        CellType::Error => (
            " t=\"e\"",
            format!("{f_xml}<v>{}</v>", escape_attr_quotes(&cell.value)),
        ),
        CellType::Text if has_formula => (
            " t=\"str\"",
            format!("{f_xml}<v>{}</v>", escape_attr_quotes(&cell.value)),
        ),
        CellType::Text => (
            " t=\"inlineStr\"",
            format!(
                r#"<is><t xml:space="preserve">{}</t></is>"#,
                escape_attr_quotes(&cell.value)
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
    let fields = crate::office_props::ooxml::core_modeled_children(|key| props.get(key).cloned());
    let ns = crate::office_props::ooxml::CORE_NS;
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties {ns}>{fields}</cp:coreProperties>"#
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
