//! Export the canonical model back to a `.docx`. Generates a minimal valid
//! `word/document.xml` from the model with the inverse mapping, re-emits every
//! preserved part verbatim, and synthesises `[Content_Types].xml` / `_rels/.rels`
//! / `word/_rels/document.xml.rels` / `word/numbering.xml` when the source had
//! none (e.g. a doc created from scratch rather than imported).

use super::container::DocxContainer;
use crate::document::MarkValue;
use crate::format::CodecError;
use crate::model::DocRef;
use crate::schema::{
    STYLE_ALIGN, STYLE_BOLD, STYLE_COLOR, STYLE_FONT, STYLE_INDENT, STYLE_ITALIC, STYLE_NAME, STYLE_SIZE, STYLE_SPACE_AFTER,
    STYLE_SPACE_BEFORE,
};
use crate::snapshot::{AttrVal, BlockView, DocSnapshot};
use std::collections::{BTreeMap, BTreeSet};

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

const CONTENT_TYPES_BASE: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>"#;

const ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;

const NUMBERING_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:numFmt w:val="bullet"/><w:lvlText w:val="&#8226;"/></w:lvl></w:abstractNum><w:abstractNum w:abstractNumId="1"><w:lvl w:ilvl="0"><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num><w:num w:numId="2"><w:abstractNumId w:val="1"/></w:num></w:numbering>"#;

const NUMBERING_OVERRIDE: &str = r#"<Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/>"#;
const NUMBERING_REL: &str = r#"<Relationship Id="rIdQdocNum" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/>"#;

const CORE_OVERRIDE: &str = r#"<Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/>"#;
const APP_OVERRIDE: &str = r#"<Override PartName="/docProps/app.xml" ContentType="application/vnd.openxmlformats-officedocument.extended-properties+xml"/>"#;
const CORE_REL: &str = r#"<Relationship Id="rIdQdocCore" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/>"#;
const APP_REL: &str = r#"<Relationship Id="rIdQdocApp" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties" Target="docProps/app.xml"/>"#;

const STYLES_OVERRIDE: &str = r#"<Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/>"#;
const STYLES_REL: &str = r#"<Relationship Id="rIdQdocStyles" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>"#;
const STYLES_XML_OPEN: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#;

pub(crate) fn export(doc: DocRef) -> Result<Vec<u8>, CodecError> {
    let snap = doc.materialize();
    let (document, named_refs, numbering) = build_document_xml(&snap);
    let has_lists = snap.blocks.iter().any(|b| b.kind == "list_item");
    let has_styles = !named_refs.is_empty();

    let mut c = DocxContainer { parts: Vec::new() };
    // Re-emit every preserved part verbatim (styles.xml, settings.xml, the
    // original content-types/rels, images, …).
    let mut had_content_types = false;
    let mut had_root_rels = false;
    let mut had_doc_rels = false;
    let mut had_numbering = false;
    let mut had_styles_part = false;
    let mut had_core = false;
    let mut had_app = false;
    let mut original_core: Option<Vec<u8>> = None;
    let mut original_app: Option<Vec<u8>> = None;
    for (key, bytes) in crate::model::preserved_parts(doc.doc, doc.c) {
        if let Some(name) = key.strip_prefix("docx/part/") {
            had_content_types |= name == "[Content_Types].xml";
            had_root_rels |= name == "_rels/.rels";
            had_doc_rels |= name == "word/_rels/document.xml.rels";
            had_numbering |= name == "word/numbering.xml";
            had_styles_part |= name == "word/styles.xml";
            if name == "docProps/core.xml" {
                had_core = true;
                original_core = Some(bytes.clone());
            }
            if name == "docProps/app.xml" {
                had_app = true;
                original_app = Some(bytes.clone());
            }
            c.set(name, bytes);
        }
    }

    c.set("word/document.xml", document.into_bytes());

    // Document properties → `docProps/core.xml` (+ `app.xml` generator). Regenerate
    // from the canonical properties, merging unmodeled children of the preserved
    // originals. A part newly created here needs a content-type override + a root
    // relationship (an imported docx already has both, so only synthesised parts
    // are wired up).
    let wrote_core = if let Some(core_xml) = super::meta::export_core(&doc, original_core.as_deref()) {
        c.set("docProps/core.xml", core_xml);
        true
    } else {
        false
    };
    let wrote_app = if let Some(app_xml) = super::meta::export_app(&doc, original_app.as_deref()) {
        c.set("docProps/app.xml", app_xml);
        true
    } else {
        false
    };
    let need_core_wireup = wrote_core && !had_core;
    let need_app_wireup = wrote_app && !had_app;

    if has_lists {
        // Ensure a numbering part exists mapping numId 1 → bullet, 2 → decimal, plus
        // any custom ordered numberings (roman/alpha and/or non-1 start) the
        // document uses, so list numbering format + start round-trip. The static
        // fallback covers the common bullet/decimal case.
        let xml = if numbering.has_custom() { numbering.numbering_xml() } else { NUMBERING_XML.to_string() };
        c.set("word/numbering.xml", xml.into_bytes());
    }

    // word/styles.xml — merge the referenced CRDT styles into the preserved part
    // (or synthesise a fresh one) as `w:style w:type="paragraph"` definitions so
    // Word/LibreOffice render the document's named styles. Styles already declared
    // in a preserved styles.xml are referenced, not redefined.
    if has_styles {
        let merged = merge_styles_xml(c.get("word/styles.xml"), &named_refs, &snap.styles);
        c.set("word/styles.xml", merged);
    }
    // A styles part referenced for the first time still needs a content-type
    // override + relationship (an imported docx already has both).
    let need_styles_wireup = has_styles && !had_styles_part;

    // [Content_Types].xml — generate when absent; otherwise ensure overrides.
    if !had_content_types {
        let mut ct = CONTENT_TYPES_BASE.to_string();
        if has_lists {
            ct.push_str(NUMBERING_OVERRIDE);
        }
        if has_styles {
            ct.push_str(STYLES_OVERRIDE);
        }
        if wrote_core {
            ct.push_str(CORE_OVERRIDE);
        }
        if wrote_app {
            ct.push_str(APP_OVERRIDE);
        }
        ct.push_str("</Types>");
        c.set("[Content_Types].xml", ct.into_bytes());
    } else {
        let mut overrides: Vec<(&str, &str)> = Vec::new();
        if has_lists && !had_numbering {
            overrides.push(("/word/numbering.xml", NUMBERING_OVERRIDE));
        }
        if need_styles_wireup {
            overrides.push(("/word/styles.xml", STYLES_OVERRIDE));
        }
        if need_core_wireup {
            overrides.push(("/docProps/core.xml", CORE_OVERRIDE));
        }
        if need_app_wireup {
            overrides.push(("/docProps/app.xml", APP_OVERRIDE));
        }
        if !overrides.is_empty() {
            if let Some(ct) = c.get("[Content_Types].xml") {
                let mut s = String::from_utf8_lossy(ct).into_owned();
                for (part, ovr) in overrides {
                    if !s.contains(part) {
                        inject_before(&mut s, "</Types>", ovr);
                    }
                }
                c.set("[Content_Types].xml", s.into_bytes());
            }
        }
    }

    // _rels/.rels — the docProps relationships live in the *root* rels (not the
    // document's). Generate fresh when absent; otherwise inject any newly-created
    // docProps relationships.
    if !had_root_rels {
        let mut s = ROOT_RELS.to_string();
        if wrote_core {
            inject_before(&mut s, "</Relationships>", CORE_REL);
        }
        if wrote_app {
            inject_before(&mut s, "</Relationships>", APP_REL);
        }
        c.set("_rels/.rels", s.into_bytes());
    } else if need_core_wireup || need_app_wireup {
        if let Some(rels) = c.get("_rels/.rels") {
            let mut s = String::from_utf8_lossy(rels).into_owned();
            if need_core_wireup && !s.contains("Target=\"docProps/core.xml\"") {
                inject_before(&mut s, "</Relationships>", CORE_REL);
            }
            if need_app_wireup && !s.contains("Target=\"docProps/app.xml\"") {
                inject_before(&mut s, "</Relationships>", APP_REL);
            }
            c.set("_rels/.rels", s.into_bytes());
        }
    }

    // word/_rels/document.xml.rels — needed once there's a numbering and/or a
    // freshly-synthesised styles part.
    if has_lists || need_styles_wireup {
        if !had_doc_rels {
            let mut inner = String::new();
            if need_styles_wireup {
                inner.push_str(STYLES_REL);
            }
            if has_lists {
                inner.push_str(NUMBERING_REL);
            }
            let rels = format!(
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{inner}</Relationships>"#
            );
            c.set("word/_rels/document.xml.rels", rels.into_bytes());
        } else if let Some(rels) = c.get("word/_rels/document.xml.rels") {
            let mut s = String::from_utf8_lossy(rels).into_owned();
            if has_lists && !s.contains("Target=\"numbering.xml\"") {
                inject_before(&mut s, "</Relationships>", NUMBERING_REL);
            }
            if need_styles_wireup && !s.contains("Target=\"styles.xml\"") {
                inject_before(&mut s, "</Relationships>", STYLES_REL);
            }
            c.set("word/_rels/document.xml.rels", s.into_bytes());
        }
    }

    c.write()
}

/// Build or extend `word/styles.xml` with `<w:style w:type="paragraph">` defs for
/// the referenced container styles. A style already declared (by `w:styleId`) in a
/// preserved styles.xml is left untouched (no duplicate; the preserved bytes win).
fn merge_styles_xml(existing: Option<&[u8]>, refs: &BTreeSet<String>, styles: &BTreeMap<String, BTreeMap<String, AttrVal>>) -> Vec<u8> {
    match existing {
        Some(bytes) => {
            let present = style_ids_in(bytes);
            let mut defs = String::new();
            for id in refs {
                if present.contains(id) {
                    continue;
                }
                if let Some(props) = styles.get(id) {
                    defs.push_str(&para_style_def_xml(id, props));
                }
            }
            if defs.is_empty() {
                return bytes.to_vec();
            }
            let mut s = String::from_utf8_lossy(bytes).into_owned();
            inject_before(&mut s, "</w:styles>", &defs);
            s.into_bytes()
        }
        None => {
            let mut s = STYLES_XML_OPEN.to_string();
            for id in refs {
                if let Some(props) = styles.get(id) {
                    s.push_str(&para_style_def_xml(id, props));
                }
            }
            s.push_str("</w:styles>");
            s.into_bytes()
        }
    }
}

/// The `w:styleId` values declared in a styles.xml, so existing styles aren't
/// redefined when merging.
fn style_ids_in(xml: &[u8]) -> BTreeSet<String> {
    let s = String::from_utf8_lossy(xml);
    let mut out = BTreeSet::new();
    let needle = "w:styleId=\"";
    let mut rest = s.as_ref();
    while let Some(p) = rest.find(needle) {
        rest = &rest[p + needle.len()..];
        if let Some(end) = rest.find('"') {
            out.insert(rest[..end].to_string());
            rest = &rest[end + 1..];
        } else {
            break;
        }
    }
    out
}

/// An OOXML `<w:style w:type="paragraph">` definition for a CRDT styles-container
/// style (`w:pPr` alignment/spacing/indent + `w:rPr` font/bold/italic/colour/size),
/// the inverse of `styles::parse_styles` + `styles::style_props`.
fn para_style_def_xml(id: &str, props: &BTreeMap<String, AttrVal>) -> String {
    let getf = |k: &str| match props.get(k) {
        Some(AttrVal::Float(f)) => Some(*f),
        Some(AttrVal::Int(n)) => Some(*n as f64),
        _ => None,
    };
    let gets = |k: &str| match props.get(k) {
        Some(AttrVal::Str(s)) => Some(s.as_str()),
        _ => None,
    };
    let getb = |k: &str| matches!(props.get(k), Some(AttrVal::Bool(true)));

    let mut pp = String::new();
    if let Some(a) = gets(STYLE_ALIGN) {
        let v = match a {
            "center" => "center",
            "right" => "right",
            "justify" => "both",
            _ => "",
        };
        if !v.is_empty() {
            pp.push_str(&format!(r#"<w:jc w:val="{v}"/>"#));
        }
    }
    let before = getf(STYLE_SPACE_BEFORE).filter(|v| *v != 0.0);
    let after = getf(STYLE_SPACE_AFTER).filter(|v| *v != 0.0);
    if before.is_some() || after.is_some() {
        pp.push_str("<w:spacing");
        if let Some(v) = before {
            pp.push_str(&format!(r#" w:before="{}""#, px_to_twips(v)));
        }
        if let Some(v) = after {
            pp.push_str(&format!(r#" w:after="{}""#, px_to_twips(v)));
        }
        pp.push_str("/>");
    }
    if let Some(v) = getf(STYLE_INDENT).filter(|v| *v != 0.0) {
        pp.push_str(&format!(r#"<w:ind w:left="{}"/>"#, px_to_twips(v)));
    }

    let mut rp = String::new();
    if let Some(f) = gets(STYLE_FONT) {
        let n = font_name(f);
        rp.push_str(&format!(r#"<w:rFonts w:ascii="{n}" w:hAnsi="{n}"/>"#));
    }
    if getb(STYLE_BOLD) {
        rp.push_str("<w:b/>");
    }
    if getb(STYLE_ITALIC) {
        rp.push_str("<w:i/>");
    }
    if let Some(c) = gets(STYLE_COLOR) {
        rp.push_str(&format!(r#"<w:color w:val="{}"/>"#, color_hex(c)));
    }
    if let Some(sz) = getf(STYLE_SIZE) {
        let hp = (sz * 2.0).round() as i64;
        rp.push_str(&format!(r#"<w:sz w:val="{hp}"/><w:szCs w:val="{hp}"/>"#));
    }

    let name = gets(STYLE_NAME).unwrap_or(id);
    let ppr = if pp.is_empty() { String::new() } else { format!("<w:pPr>{pp}</w:pPr>") };
    let rpr = if rp.is_empty() { String::new() } else { format!("<w:rPr>{rp}</w:rPr>") };
    format!(
        r#"<w:style w:type="paragraph" w:styleId="{}"><w:name w:val="{}"/>{ppr}{rpr}</w:style>"#,
        escape_attr(id),
        escape_attr(name)
    )
}

/// Pixels (96 dpi) → OOXML twips (1/1440 inch): the inverse of
/// `styles::twips_to_px`. 1 px = 15 twips.
fn px_to_twips(px: f64) -> i64 {
    (px * 15.0).round() as i64
}

/// `#rrggbb` → OOXML `RRGGBB` (no hash, upper-case as Word writes it). Re-imports
/// to the same canonical `#rrggbb` via `styles::normalize_color`.
fn color_hex(c: &str) -> String {
    c.trim_start_matches('#').to_ascii_uppercase()
}

/// Escape a string for use inside a double-quoted XML attribute value.
fn escape_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// Insert `frag` immediately before the first occurrence of `needle`.
fn inject_before(s: &mut String, needle: &str, frag: &str) {
    if let Some(pos) = s.rfind(needle) {
        s.insert_str(pos, frag);
    }
}

fn push_escaped(out: &mut String, c: char) {
    match c {
        '&' => out.push_str("&amp;"),
        '<' => out.push_str("&lt;"),
        '>' => out.push_str("&gt;"),
        _ => out.push(c),
    }
}

/// Concrete font name for a generic family (round-trips via `generic_font`).
fn font_name(generic: &str) -> &'static str {
    match generic {
        "serif" => "Times New Roman",
        "mono" => "Courier New",
        _ => "Arial",
    }
}

/// The active marks covering character `i`, sorted by name for stable grouping.
fn marks_at(i: usize, marks: &[crate::document::MarkSpan]) -> Vec<(String, MarkValue)> {
    let mut v: Vec<(String, MarkValue)> = marks
        .iter()
        .filter(|m| i >= m.start && i < m.end)
        .map(|m| (m.name.clone(), m.value.clone()))
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

/// Build the `<w:rPr>` body for an active mark set (empty if none modeled).
fn run_props(marks: &[(String, MarkValue)]) -> String {
    let (mut b, mut i, mut u, mut strike) = (false, false, false, false);
    let mut sz: Option<i64> = None;
    let mut font: Option<&str> = None;
    let mut color: Option<&str> = None;
    for (name, val) in marks {
        match name.as_str() {
            "strong" => b = true,
            "em" => i = true,
            "underline" => u = true,
            "strike" => strike = true,
            "fontSize" => sz = val.as_int(),
            "fontFamily" => font = val.as_str(),
            "color" => color = val.as_str(),
            _ => {}
        }
    }
    let mut s = String::new();
    if let Some(f) = font {
        let n = font_name(f);
        s.push_str(&format!(r#"<w:rFonts w:ascii="{n}" w:hAnsi="{n}"/>"#));
    }
    if b {
        s.push_str("<w:b/>");
    }
    if i {
        s.push_str("<w:i/>");
    }
    if strike {
        s.push_str("<w:strike/>");
    }
    if let Some(c) = color {
        s.push_str(&format!(r#"<w:color w:val="{}"/>"#, color_hex(c)));
    }
    if let Some(n) = sz {
        let hp = n * 2;
        s.push_str(&format!(r#"<w:sz w:val="{hp}"/><w:szCs w:val="{hp}"/>"#));
    }
    if u {
        s.push_str(r#"<w:u w:val="single"/>"#);
    }
    s
}

/// Emit run content, turning `\t` → `<w:tab/>` and `\n` → `<w:br/>`.
fn run_content(chars: &[char]) -> String {
    let mut out = String::new();
    let mut buf = String::new();
    let flush = |out: &mut String, buf: &mut String| {
        if !buf.is_empty() {
            out.push_str(r#"<w:t xml:space="preserve">"#);
            for c in buf.chars() {
                push_escaped(out, c);
            }
            out.push_str("</w:t>");
            buf.clear();
        }
    };
    for &c in chars {
        match c {
            '\t' => {
                flush(&mut out, &mut buf);
                out.push_str("<w:tab/>");
            }
            '\n' => {
                flush(&mut out, &mut buf);
                out.push_str("<w:br/>");
            }
            _ => buf.push(c),
        }
    }
    flush(&mut out, &mut buf);
    out
}

/// Render a block's text + marks as a sequence of `<w:r>` runs. A new run starts
/// whenever the active mark set changes, so output is always well-formed.
fn render_runs(text: &str, marks: &[crate::document::MarkSpan]) -> String {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    if n == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut i = 0;
    while i < n {
        let active = marks_at(i, marks);
        let mut j = i + 1;
        while j < n && marks_at(j, marks) == active {
            j += 1;
        }
        let rpr = run_props(&active);
        out.push_str("<w:r>");
        if !rpr.is_empty() {
            out.push_str(&format!("<w:rPr>{rpr}</w:rPr>"));
        }
        out.push_str(&run_content(&chars[i..j]));
        out.push_str("</w:r>");
        i = j;
    }
    out
}

fn jc_xml(b: &BlockView) -> String {
    match b.attrs.get("align") {
        Some(AttrVal::Str(a)) => match a.as_str() {
            "center" => r#"<w:jc w:val="center"/>"#.into(),
            "right" => r#"<w:jc w:val="right"/>"#.into(),
            "justify" => r#"<w:jc w:val="both"/>"#.into(),
            _ => String::new(),
        },
        _ => String::new(),
    }
}

fn cell_table(b: &BlockView) -> Option<String> {
    match b.attrs.get("table") {
        Some(AttrVal::Str(s)) => Some(s.clone()),
        _ => None,
    }
}

fn cell_rc(b: &BlockView) -> (usize, usize) {
    let g = |k: &str| match b.attrs.get(k) {
        Some(AttrVal::Int(n)) if *n >= 0 => *n as usize,
        _ => 0,
    };
    (g("row"), g("col"))
}

/// Map the model's canonical numbering format onto an OOXML `w:numFmt w:val`
/// (inverse of `styles::map_num_format`).
fn ooxml_num_format(fmt: &str) -> &'static str {
    match fmt {
        "lower-roman" => "lowerRoman",
        "upper-roman" => "upperRoman",
        "lower-alpha" => "lowerLetter",
        "upper-alpha" => "upperLetter",
        _ => "decimal",
    }
}

/// Collects the distinct list numberings a document uses and assigns each a stable
/// `w:numId`, so an ordered list's format + start round-trip. numId 1 is the
/// bullet list and numId 2 the plain decimal-from-1 list (matching the static
/// fallback); non-default ordered numberings get fresh ids from 3 up.
#[derive(Default)]
struct Numbering {
    /// `(ordered, format, start)` → assigned numId.
    ids: Vec<((bool, String, i64), i64)>,
}

impl Numbering {
    /// The `w:numId` for a list item, registering a new id if the numbering is
    /// novel. Bullet and plain decimal-from-1 reuse the fixed ids 1 / 2.
    fn num_id(&mut self, b: &BlockView) -> i64 {
        let ordered = matches!(b.attrs.get("ordered"), Some(AttrVal::Bool(true)));
        if !ordered {
            return 1;
        }
        let fmt = match b.attrs.get("num_format") {
            Some(AttrVal::Str(s)) => s.clone(),
            _ => "decimal".to_string(),
        };
        let start = match b.attrs.get("num_start") {
            Some(AttrVal::Int(n)) => *n,
            _ => 1,
        };
        if fmt == "decimal" && start == 1 {
            return 2;
        }
        let key = (ordered, fmt, start);
        if let Some((_, id)) = self.ids.iter().find(|(k, _)| *k == key) {
            return *id;
        }
        let id = 3 + self.ids.len() as i64;
        self.ids.push((key, id));
        id
    }

    /// Whether any non-default ordered numbering was registered (so the exporter
    /// generates a custom numbering.xml rather than the static fallback).
    fn has_custom(&self) -> bool {
        !self.ids.is_empty()
    }

    /// The full `word/numbering.xml` reconstructing every numbering this document
    /// uses: bullet (1), decimal-from-1 (2), and each registered custom ordered
    /// numbering with its `w:numFmt` + `w:start`.
    fn numbering_xml(&self) -> String {
        let mut abs = String::new();
        let mut nums = String::new();
        // Fixed: abstract 0 = bullet → numId 1; abstract 1 = decimal → numId 2.
        abs.push_str(r#"<w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:numFmt w:val="bullet"/><w:lvlText w:val="&#8226;"/></w:lvl></w:abstractNum>"#);
        abs.push_str(r#"<w:abstractNum w:abstractNumId="1"><w:lvl w:ilvl="0"><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl></w:abstractNum>"#);
        nums.push_str(r#"<w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>"#);
        nums.push_str(r#"<w:num w:numId="2"><w:abstractNumId w:val="1"/></w:num>"#);
        for ((_, fmt, start), id) in &self.ids {
            let nf = ooxml_num_format(fmt);
            abs.push_str(&format!(
                r#"<w:abstractNum w:abstractNumId="{id}"><w:lvl w:ilvl="0"><w:start w:val="{start}"/><w:numFmt w:val="{nf}"/><w:lvlText w:val="%1."/></w:lvl></w:abstractNum>"#
            ));
            nums.push_str(&format!(r#"<w:num w:numId="{id}"><w:abstractNumId w:val="{id}"/></w:num>"#));
        }
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">{abs}{nums}</w:numbering>"#
        )
    }
}

/// Build `word/document.xml` and collect the set of CRDT styles-container styleIds
/// referenced by a `<w:pStyle>` (so the caller can emit their definitions), plus
/// the list-numbering plan (so the caller can write a matching numbering.xml).
fn build_document_xml(snap: &DocSnapshot) -> (String, BTreeSet<String>, Numbering) {
    let mut body = String::new();
    let mut named_refs: BTreeSet<String> = BTreeSet::new();
    let mut numbering = Numbering::default();
    let mut i = 0;
    while i < snap.blocks.len() {
        let b = &snap.blocks[i];
        match b.kind.as_str() {
            "heading" => {
                let level = match b.attrs.get("level") {
                    Some(AttrVal::Int(n)) => (*n).clamp(1, 6),
                    _ => 1,
                };
                body.push_str(&format!(
                    r#"<w:p><w:pPr><w:pStyle w:val="Heading{level}"/>{}</w:pPr>{}</w:p>"#,
                    jc_xml(b),
                    render_runs(&b.text, &b.marks)
                ));
                i += 1;
            }
            "list_item" => {
                let numid = numbering.num_id(b);
                body.push_str(&format!(
                    r#"<w:p><w:pPr><w:pStyle w:val="ListParagraph"/><w:numPr><w:ilvl w:val="0"/><w:numId w:val="{numid}"/></w:numPr>{}</w:pPr>{}</w:p>"#,
                    jc_xml(b),
                    render_runs(&b.text, &b.marks)
                ));
                i += 1;
            }
            "fidelity" => {
                if let Some(AttrVal::Str(xml)) = b.attrs.get("xml") {
                    body.push_str(xml);
                }
                i += 1;
            }
            "cell" => {
                // Emit a run of same-`table` cells as a real OOXML table.
                let tid = cell_table(b).unwrap_or_default();
                let start = i;
                while i < snap.blocks.len() && cell_table(&snap.blocks[i]).as_deref() == Some(tid.as_str()) {
                    i += 1;
                }
                let cells = &snap.blocks[start..i];
                let nrows = cells.iter().map(|c| cell_rc(c).0).max().unwrap_or(0) + 1;
                let ncols = cells.iter().map(|c| cell_rc(c).1).max().unwrap_or(0) + 1;
                body.push_str(r#"<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid>"#);
                for _ in 0..ncols {
                    body.push_str("<w:gridCol/>");
                }
                body.push_str("</w:tblGrid>");
                for r in 0..nrows {
                    body.push_str("<w:tr>");
                    for c in 0..ncols {
                        let inner = cells
                            .iter()
                            .find(|cl| cell_rc(cl) == (r, c))
                            .map(|cl| render_runs(&cl.text, &cl.marks))
                            .unwrap_or_default();
                        body.push_str(&format!(
                            r#"<w:tc><w:tcPr><w:tcW w:w="0" w:type="auto"/></w:tcPr><w:p>{inner}</w:p></w:tc>"#
                        ));
                    }
                    body.push_str("</w:tr>");
                }
                body.push_str("</w:tbl>");
            }
            _ => {
                // A plain paragraph. A `style` attr referencing a container style
                // emits a `<w:pStyle>` (and registers the styleId for definition);
                // a direct `align` attr still rides as a `<w:jc>`.
                let mut ppr_inner = String::new();
                if let Some(AttrVal::Str(sid)) = b.attrs.get("style") {
                    if snap.styles.contains_key(sid) {
                        ppr_inner.push_str(&format!(r#"<w:pStyle w:val="{}"/>"#, escape_attr(sid)));
                        named_refs.insert(sid.clone());
                    }
                }
                ppr_inner.push_str(&jc_xml(b));
                let ppr = if ppr_inner.is_empty() { String::new() } else { format!("<w:pPr>{ppr_inner}</w:pPr>") };
                body.push_str(&format!("<w:p>{ppr}{}</w:p>", render_runs(&b.text, &b.marks)));
                i += 1;
            }
        }
    }

    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W_NS}" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:o="urn:schemas-microsoft-com:office:office" xmlns:v="urn:schemas-microsoft-com:vml" xmlns:w10="urn:schemas-microsoft-com:office:word" xmlns:wne="http://schemas.microsoft.com/office/word/2006/wordml"><w:body>{body}</w:body></w:document>"#
    );
    (document, named_refs, numbering)
}

#[cfg(test)]
mod tests {
    use super::super::container::{minimal_docx_styled, DocxContainer};
    use super::super::DocxCodec;
    use crate::document::MarkValue;
    use crate::{AttrVal, DocSnapshot, Document, FormatProvider};
    use std::collections::BTreeMap;

    type Norm = Vec<(String, String, Vec<(usize, usize, String)>, BTreeMap<String, AttrVal>)>;

    fn norm(snap: &DocSnapshot) -> Norm {
        snap.blocks
            .iter()
            .map(|b| {
                let mut m: Vec<_> = b.marks.iter().map(|x| (x.start, x.end, x.name.clone())).collect();
                m.sort();
                (b.kind.clone(), b.text.clone(), m, b.attrs.clone())
            })
            .collect()
    }

    #[test]
    fn export_import_roundtrips_model() {
        let body = concat!(
            r#"<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Title</w:t></w:r></w:p>"#,
            r#"<w:p><w:r><w:t xml:space="preserve">a </w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t>bold</w:t></w:r><w:r><w:t xml:space="preserve"> b</w:t></w:r></w:p>"#,
        );
        let docx = minimal_docx_styled("", body);
        let mut d = Document::new();
        DocxCodec.import(&docx, &mut d.as_mut()).unwrap();
        let m1 = norm(&d.materialize());

        let exported = DocxCodec.export(d.as_ref()).unwrap();
        let c = DocxContainer::read(&exported).unwrap();
        assert!(c.get("word/document.xml").is_some());
        assert!(c.get("[Content_Types].xml").is_some());
        assert!(c.get("_rels/.rels").is_some());

        let mut d2 = Document::new();
        DocxCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let m2 = norm(&d2.materialize());
        assert_eq!(m1, m2, "model round-trips through .docx export then import");
    }

    #[test]
    fn heading_paragraph_props_roundtrip() {
        let mut d = Document::new();
        let h = d.insert_block(None, "heading");
        d.splice_text(&h, 0, 0, "Head");
        let p = d.insert_block(Some(&h), "paragraph");
        d.splice_text(&p, 0, 0, "styled");
        {
            let mut dm = d.as_mut();
            dm.set_attr(&h, "level", 3_i64);
            dm.set_attr(&p, "align", "center");
            dm.add_mark(&p, 0, 6, "fontSize", 22_i64);
            dm.add_mark(&p, 0, 6, "fontFamily", "serif");
            dm.add_mark(&p, 0, 6, "underline", true);
        }

        let exported = DocxCodec.export(d.as_ref()).unwrap();
        let mut d2 = Document::new();
        DocxCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let snap = d2.materialize();
        assert_eq!(snap.blocks[0].kind, "heading");
        assert_eq!(snap.blocks[0].attrs.get("level"), Some(&AttrVal::Int(3)));
        let b = &snap.blocks[1];
        assert_eq!(b.text, "styled");
        assert_eq!(b.attrs.get("align"), Some(&AttrVal::Str("center".into())));
        assert!(b.marks.iter().any(|m| m.name == "fontSize" && m.value == MarkValue::Int(22)), "{:?}", b.marks);
        assert!(b.marks.iter().any(|m| m.name == "fontFamily" && m.value == MarkValue::Str("serif".into())), "{:?}", b.marks);
        assert!(b.marks.iter().any(|m| m.name == "underline"), "{:?}", b.marks);
    }

    #[test]
    fn lists_roundtrip_with_ordering() {
        let mut d = Document::new();
        let a = d.insert_block(None, "list_item");
        d.splice_text(&a, 0, 0, "first");
        let b = d.insert_block(Some(&a), "list_item");
        d.splice_text(&b, 0, 0, "second");
        {
            let mut dm = d.as_mut();
            dm.set_attr(&a, "ordered", true);
            dm.set_attr(&b, "ordered", false);
        }

        let exported = DocxCodec.export(d.as_ref()).unwrap();
        let mut d2 = Document::new();
        DocxCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let snap = d2.materialize();
        let items: Vec<_> = snap.blocks.iter().filter(|x| x.kind == "list_item").collect();
        assert_eq!(items.len(), 2, "{snap:?}");
        assert_eq!(items[0].text, "first");
        assert_eq!(items[0].attrs.get("ordered"), Some(&AttrVal::Bool(true)));
        assert_eq!(items[1].attrs.get("ordered"), Some(&AttrVal::Bool(false)));
    }

    #[test]
    fn list_numbering_format_and_start_roundtrip_through_docx() {
        // An upper-roman ordered list starting at 3: format + start must survive a
        // .docx export then re-import (fidelity preservation).
        let mut d = Document::new();
        let a = d.insert_block(None, "list_item");
        d.splice_text(&a, 0, 0, "three");
        let b = d.insert_block(Some(&a), "list_item");
        d.splice_text(&b, 0, 0, "four");
        {
            let mut dm = d.as_mut();
            for id in [&a, &b] {
                dm.set_attr(id, "ordered", true);
                dm.set_attr(id, "num_format", "upper-roman");
                dm.set_attr(id, "num_start", 3_i64);
                dm.set_attr(id, "num_restart", true);
            }
        }

        let exported = DocxCodec.export(d.as_ref()).unwrap();
        let cont = DocxContainer::read(&exported).unwrap();
        let numbering = std::str::from_utf8(cont.get("word/numbering.xml").unwrap()).unwrap();
        assert!(numbering.contains(r#"<w:numFmt w:val="upperRoman"/>"#), "upper-roman numFmt emitted: {numbering}");
        assert!(numbering.contains(r#"<w:start w:val="3"/>"#), "start value emitted: {numbering}");

        let mut d2 = Document::new();
        DocxCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let snap = d2.materialize();
        let items: Vec<_> = snap.blocks.iter().filter(|x| x.kind == "list_item").collect();
        assert_eq!(items.len(), 2, "{snap:?}");
        for it in &items {
            assert_eq!(it.attrs.get("ordered"), Some(&AttrVal::Bool(true)), "still ordered: {:?}", it.attrs);
            assert_eq!(it.attrs.get("num_format"), Some(&AttrVal::Str("upper-roman".into())), "format survives: {:?}", it.attrs);
            assert_eq!(it.attrs.get("num_start"), Some(&AttrVal::Int(3)), "start survives: {:?}", it.attrs);
        }

        // Idempotent: a second round-trip preserves the same numbering attrs.
        let exported2 = DocxCodec.export(d2.as_ref()).unwrap();
        let mut d3 = Document::new();
        DocxCodec.import(&exported2, &mut d3.as_mut()).unwrap();
        assert_eq!(norm(&d2.materialize()), norm(&d3.materialize()), "list-numbering model stable across a second round-trip");
    }

    #[test]
    fn colored_serif_20pt_justified_run_roundtrips_through_docx() {
        // The headline task: a coloured + serif + 20pt run in a justified
        // paragraph must round-trip export→import with every attribute intact.
        let mut d = Document::new();
        let p = d.insert_block(None, "paragraph");
        d.splice_text(&p, 0, 0, "styled");
        {
            let mut dm = d.as_mut();
            dm.set_attr(&p, "align", "justify");
            dm.add_mark(&p, 0, 6, "color", "#3366cc");
            dm.add_mark(&p, 0, 6, "fontFamily", "serif");
            dm.add_mark(&p, 0, 6, "fontSize", 20_i64);
        }
        let exported = DocxCodec.export(d.as_ref()).unwrap();
        let cont = DocxContainer::read(&exported).unwrap();
        let document = std::str::from_utf8(cont.get("word/document.xml").unwrap()).unwrap();
        assert!(document.contains(r#"<w:jc w:val="both"/>"#), "justify → w:jc both: {document}");

        let mut d2 = Document::new();
        DocxCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let b = &d2.materialize().blocks[0];
        assert_eq!(b.text, "styled");
        assert_eq!(b.attrs.get("align"), Some(&AttrVal::Str("justify".into())), "justify survives: {:?}", b.attrs);
        assert!(b.marks.iter().any(|m| m.name == "color" && m.value == MarkValue::Str("#3366cc".into())), "colour: {:?}", b.marks);
        assert!(b.marks.iter().any(|m| m.name == "fontFamily" && m.value == MarkValue::Str("serif".into())), "serif: {:?}", b.marks);
        assert!(b.marks.iter().any(|m| m.name == "fontSize" && m.value == MarkValue::Int(20)), "20pt: {:?}", b.marks);
    }

    #[test]
    fn table_roundtrips_as_editable_cells() {
        let mut d = Document::new();
        let mut prev: Option<String> = None;
        for (r, c, t) in [(0, 0, "a"), (0, 1, "b"), (1, 0, "c"), (1, 1, "d")] {
            let id = d.insert_block(prev.as_deref(), "cell");
            d.splice_text(&id, 0, 0, t);
            {
                let mut dm = d.as_mut();
                dm.set_attr(&id, "table", "T1");
                dm.set_attr(&id, "row", r as i64);
                dm.set_attr(&id, "col", c as i64);
            }
            prev = Some(id);
        }
        let exported = DocxCodec.export(d.as_ref()).unwrap();
        let mut d2 = Document::new();
        DocxCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let snap = d2.materialize();
        let cells: Vec<_> = snap.blocks.iter().filter(|b| b.kind == "cell").collect();
        assert_eq!(cells.iter().map(|c| c.text.clone()).collect::<Vec<_>>(), vec!["a", "b", "c", "d"], "{snap:?}");
        assert_eq!(cells[3].attrs.get("row"), Some(&AttrVal::Int(1)));
        assert_eq!(cells[3].attrs.get("col"), Some(&AttrVal::Int(1)));
        // Cells stay editable: their text lives in a real Text object.
        let id = &cells[0].id;
        d2.splice_text(id, 1, 0, "X");
        assert_eq!(d2.block_text(id).as_deref(), Some("aX"));
    }

    #[test]
    fn named_paragraph_style_roundtrips_through_docx() {
        // A paragraph that references a custom CRDT-native named style must export
        // as a real OOXML `w:style w:type="paragraph"` + `w:pStyle`, and re-import
        // with the style intact and still applied to the block. Mirrors the `.odt`
        // named-style round-trip.
        let mut d = Document::new();
        let p = d.insert_block(None, "paragraph");
        d.splice_text(&p, 0, 0, "fancy");
        {
            let mut dm = d.as_mut();
            let mut props = BTreeMap::new();
            props.insert("name".to_string(), AttrVal::Str("Fancy".into()));
            props.insert("font".to_string(), AttrVal::Str("serif".into()));
            props.insert("size".to_string(), AttrVal::Float(20.0));
            props.insert("bold".to_string(), AttrVal::Bool(true));
            props.insert("italic".to_string(), AttrVal::Bool(true));
            props.insert("color".to_string(), AttrVal::Str("#ff0000".into()));
            props.insert("align".to_string(), AttrVal::Str("center".into()));
            props.insert("space_after".to_string(), AttrVal::Float(12.0));
            dm.set_style("Fancy", &props);
            dm.set_attr(&p, "style", "Fancy");
        }

        let exported = DocxCodec.export(d.as_ref()).unwrap();
        let cont = DocxContainer::read(&exported).unwrap();
        let document = std::str::from_utf8(cont.get("word/document.xml").unwrap()).unwrap();
        assert!(document.contains(r#"<w:pStyle w:val="Fancy"/>"#), "paragraph references the named style: {document}");
        let styles = std::str::from_utf8(cont.get("word/styles.xml").unwrap()).unwrap();
        assert!(
            styles.contains(r#"<w:style w:type="paragraph" w:styleId="Fancy">"#),
            "named style defined as OOXML w:style: {styles}"
        );
        assert!(
            styles.contains("<w:b/>") && styles.contains("<w:i/>") && styles.contains(r#"<w:sz w:val="40"/>"#),
            "props emitted: {styles}"
        );
        assert!(styles.contains(r#"<w:jc w:val="center"/>"#) && styles.contains(r#"<w:color w:val="FF0000"/>"#), "pPr/colour emitted: {styles}");
        // The freshly-synthesised styles part is wired into content-types + rels.
        let ct = std::str::from_utf8(cont.get("[Content_Types].xml").unwrap()).unwrap();
        assert!(ct.contains("/word/styles.xml"), "styles override registered: {ct}");
        let rels = std::str::from_utf8(cont.get("word/_rels/document.xml.rels").unwrap()).unwrap();
        assert!(rels.contains(r#"Target="styles.xml""#), "styles relationship registered: {rels}");

        let mut d2 = Document::new();
        DocxCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let snap = d2.materialize();
        let b = &snap.blocks[0];
        assert_eq!(b.text, "fancy");
        assert_eq!(b.attrs.get("style"), Some(&AttrVal::Str("Fancy".into())), "block keeps its style ref: {:?}", b.attrs);

        let st = snap.styles.get("Fancy").expect("style present in container after round-trip");
        assert_eq!(st.get("name"), Some(&AttrVal::Str("Fancy".into())));
        assert_eq!(st.get("font"), Some(&AttrVal::Str("serif".into())));
        assert_eq!(st.get("size"), Some(&AttrVal::Float(20.0)));
        assert_eq!(st.get("bold"), Some(&AttrVal::Bool(true)));
        assert_eq!(st.get("italic"), Some(&AttrVal::Bool(true)));
        assert_eq!(st.get("color"), Some(&AttrVal::Str("#ff0000".into())));
        assert_eq!(st.get("align"), Some(&AttrVal::Str("center".into())));
        assert_eq!(st.get("space_after"), Some(&AttrVal::Float(12.0)));

        // The round-trip is idempotent: exporting d2 yields the same model again.
        let exported2 = DocxCodec.export(d2.as_ref()).unwrap();
        let mut d3 = Document::new();
        DocxCodec.import(&exported2, &mut d3.as_mut()).unwrap();
        assert_eq!(norm(&d2.materialize()), norm(&d3.materialize()), "named-style model is stable across a second round-trip");
    }

    #[test]
    fn document_properties_roundtrip_through_docx() {
        // Title/author/subject/keywords/dates/generator on a fresh doc must export
        // into docProps/core.xml (+ app.xml generator) and re-import unchanged.
        let mut d = Document::new();
        let p = d.insert_block(None, "paragraph");
        d.splice_text(&p, 0, 0, "body");
        d.set_doc_prop("title", "My Title");
        d.set_doc_prop("author", "Ada Lovelace");
        d.set_doc_prop("subject", "Analytical Engine");
        d.set_doc_prop("keywords", "computing, history");
        d.set_doc_prop("created", "2020-01-02T03:04:05Z");
        d.set_doc_prop("modified", "2021-06-07T08:09:10Z");
        d.set_doc_prop("generator", "qdoc/1.0");

        let exported = DocxCodec.export(d.as_ref()).unwrap();
        let cont = DocxContainer::read(&exported).unwrap();
        let core = std::str::from_utf8(cont.get("docProps/core.xml").expect("core.xml written")).unwrap();
        assert!(core.contains("<dc:title>My Title</dc:title>"), "title in core.xml: {core}");
        assert!(core.contains("<dc:creator>Ada Lovelace</dc:creator>"), "creator: {core}");
        assert!(core.contains("<dc:subject>Analytical Engine</dc:subject>"), "subject: {core}");
        assert!(core.contains("<cp:keywords>computing, history</cp:keywords>"), "keywords: {core}");
        assert!(core.contains("2020-01-02T03:04:05Z</dcterms:created>"), "created: {core}");
        assert!(core.contains("2021-06-07T08:09:10Z</dcterms:modified>"), "modified: {core}");
        let app = std::str::from_utf8(cont.get("docProps/app.xml").expect("app.xml written")).unwrap();
        assert!(app.contains("<Application>qdoc/1.0</Application>"), "generator in app.xml: {app}");
        // Freshly-synthesised parts are wired into content-types + the root rels.
        let ct = std::str::from_utf8(cont.get("[Content_Types].xml").unwrap()).unwrap();
        assert!(ct.contains("/docProps/core.xml") && ct.contains("/docProps/app.xml"), "overrides registered: {ct}");
        let rels = std::str::from_utf8(cont.get("_rels/.rels").unwrap()).unwrap();
        assert!(rels.contains(r#"Target="docProps/core.xml""#) && rels.contains(r#"Target="docProps/app.xml""#), "rels registered: {rels}");

        let mut d2 = Document::new();
        DocxCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let props = d2.doc_props();
        assert_eq!(props.get("title").map(String::as_str), Some("My Title"));
        assert_eq!(props.get("author").map(String::as_str), Some("Ada Lovelace"));
        assert_eq!(props.get("subject").map(String::as_str), Some("Analytical Engine"));
        assert_eq!(props.get("keywords").map(String::as_str), Some("computing, history"));
        assert_eq!(props.get("created").map(String::as_str), Some("2020-01-02T03:04:05Z"));
        assert_eq!(props.get("modified").map(String::as_str), Some("2021-06-07T08:09:10Z"));
        assert_eq!(props.get("generator").map(String::as_str), Some("qdoc/1.0"));
        assert_eq!(d2.materialize().blocks[0].text, "body");

        // Idempotent across a second round-trip.
        let exported2 = DocxCodec.export(d2.as_ref()).unwrap();
        let mut d3 = Document::new();
        DocxCodec.import(&exported2, &mut d3.as_mut()).unwrap();
        assert_eq!(d2.doc_props(), d3.doc_props(), "properties stable across a second round-trip");
    }

    #[test]
    fn core_xml_preserves_unmodeled_fields_and_updates_modeled() {
        // An imported core.xml with an unmodeled field (revision) plus a title:
        // the unmodeled field survives and the title updates on export.
        let core = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/"><dc:title>Old</dc:title><cp:revision>4</cp:revision></cp:coreProperties>"#;
        let base = minimal_docx_styled("", "<w:p><w:r><w:t>hi</w:t></w:r></w:p>");
        let mut c = DocxContainer::read(&base).unwrap();
        c.set("docProps/core.xml", core.as_bytes().to_vec());
        let docx = c.write().unwrap();

        let mut d = Document::new();
        DocxCodec.import(&docx, &mut d.as_mut()).unwrap();
        assert_eq!(d.doc_prop("title").as_deref(), Some("Old"), "title imported from core.xml");
        d.set_doc_prop("title", "New");

        let exported = DocxCodec.export(d.as_ref()).unwrap();
        let out = DocxContainer::read(&exported).unwrap();
        let core_out = std::str::from_utf8(out.get("docProps/core.xml").unwrap()).unwrap();
        assert!(core_out.contains("<dc:title>New</dc:title>"), "title updated: {core_out}");
        assert!(!core_out.contains("<dc:title>Old</dc:title>"), "old title gone: {core_out}");
        assert!(core_out.contains("revision>4</"), "unmodeled field preserved: {core_out}");
    }

    #[test]
    fn no_docprops_when_no_properties_and_none_imported() {
        // A from-scratch doc with no properties must not synthesise docProps parts.
        let mut d = Document::new();
        let p = d.insert_block(None, "paragraph");
        d.splice_text(&p, 0, 0, "plain");
        let exported = DocxCodec.export(d.as_ref()).unwrap();
        let cont = DocxContainer::read(&exported).unwrap();
        assert!(cont.get("docProps/core.xml").is_none(), "no core.xml for a doc with no properties");
        assert!(cont.get("docProps/app.xml").is_none(), "no app.xml for a doc with no properties");
    }

    #[test]
    fn preserves_unmodeled_parts() {
        let base = minimal_docx_styled("", "<w:p><w:r><w:t>hi</w:t></w:r></w:p>");
        let mut c = DocxContainer::read(&base).unwrap();
        c.set("customXml/item1.xml", b"<root>custom</root>".to_vec());
        let docx = c.write().unwrap();

        let mut d = Document::new();
        DocxCodec.import(&docx, &mut d.as_mut()).unwrap();
        let exported = DocxCodec.export(d.as_ref()).unwrap();

        let out = DocxContainer::read(&exported).unwrap();
        assert_eq!(
            out.get("customXml/item1.xml"),
            Some(&b"<root>custom</root>"[..]),
            "unmodeled part must round-trip byte-identically"
        );
        assert!(out.get("word/document.xml").is_some(), "body regenerated");
        // styles.xml from the source survives verbatim.
        assert_eq!(out.get("word/styles.xml"), c.get("word/styles.xml"));
    }
}
