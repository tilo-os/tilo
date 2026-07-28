//! Page header / footer ⇄ OOXML header/footer parts. WordprocessingML keeps each
//! page region in its own part (`word/header1.xml` = `w:hdr`, `word/footer1.xml` =
//! `w:ftr`), referenced from the body's `w:sectPr` via `w:headerReference` /
//! `w:footerReference` (by relationship id) and declared in `[Content_Types].xml`
//! + `word/_rels/document.xml.rels`. A page-number field is a `PAGE` field, emitted
//! as a `w:fldSimple w:instr=" PAGE "` (the simple form) and parsed from both the
//! simple form and the complex `w:instrText` form. This maps the header/footer
//! paragraph sequences onto the canonical regions, both directions.

use crate::model::{materialize_region, DocMut, DocRef, Region};
use crate::schema::{FIELD_PAGE_NUMBER, FIELD_PLACEHOLDER, MARK_FIELD};
use crate::snapshot::BlockView;
use quick_xml::events::Event;
use quick_xml::reader::Reader;

/// The fixed part names + relationship ids qdoc writes for header/footer.
pub(super) const HEADER_PART: &str = "word/header1.xml";
pub(super) const FOOTER_PART: &str = "word/footer1.xml";
pub(super) const HEADER_REL_ID: &str = "rIdQdocHdr";
pub(super) const FOOTER_REL_ID: &str = "rIdQdocFtr";

/// Resolve the default header + footer part names referenced by the body's first
/// `w:sectPr`, via `word/_rels/document.xml.rels`. Returns `(header_part,
/// footer_part)` part names (paths under the package root, e.g.
/// `"word/header1.xml"`), each `None` when no default reference exists. Prefers the
/// `w:type="default"` reference; falls back to the first reference of each kind.
pub(super) fn resolve_default_parts(document_xml: &[u8], rels_xml: Option<&[u8]>) -> (Option<String>, Option<String>) {
    let (hdr_rid, ftr_rid) = sectpr_reference_ids(document_xml);
    let rels = rels_xml.map(rel_targets).unwrap_or_default();
    let resolve = |rid: Option<String>| -> Option<String> {
        let rid = rid?;
        let target = rels.iter().find(|(id, _)| *id == rid).map(|(_, t)| t.clone())?;
        Some(normalize_target(&target))
    };
    (resolve(hdr_rid), resolve(ftr_rid))
}

/// The default (or first) header/footer relationship ids from the first
/// `w:sectPr` in the document body.
fn sectpr_reference_ids(document_xml: &[u8]) -> (Option<String>, Option<String>) {
    let Ok(s) = std::str::from_utf8(document_xml) else { return (None, None) };
    let mut reader = Reader::from_str(s);
    let mut in_sectpr = false;
    let mut hdr_default: Option<String> = None;
    let mut hdr_first: Option<String> = None;
    let mut ftr_default: Option<String> = None;
    let mut ftr_first: Option<String> = None;
    loop {
        let ev = match reader.read_event() {
            Ok(e) => e,
            Err(_) => break,
        };
        match ev {
            Event::Start(e) if e.name().as_ref() == b"w:sectPr" => in_sectpr = true,
            Event::Empty(e) | Event::Start(e) if in_sectpr => {
                match e.name().as_ref() {
                    b"w:headerReference" => {
                        let rid = attr_r_id(&e);
                        let kind = attr_w_val(&e, b"w:type");
                        if hdr_first.is_none() {
                            hdr_first = rid.clone();
                        }
                        if kind.as_deref() == Some("default") {
                            hdr_default = rid;
                        }
                    }
                    b"w:footerReference" => {
                        let rid = attr_r_id(&e);
                        let kind = attr_w_val(&e, b"w:type");
                        if ftr_first.is_none() {
                            ftr_first = rid.clone();
                        }
                        if kind.as_deref() == Some("default") {
                            ftr_default = rid;
                        }
                    }
                    _ => {}
                }
            }
            Event::End(e) if e.name().as_ref() == b"w:sectPr" => in_sectpr = false,
            Event::Eof => break,
            _ => {}
        }
    }
    (hdr_default.or(hdr_first), ftr_default.or(ftr_first))
}

/// `(Id, Target)` pairs from a `word/_rels/document.xml.rels`.
fn rel_targets(rels_xml: &[u8]) -> Vec<(String, String)> {
    let Ok(s) = std::str::from_utf8(rels_xml) else { return Vec::new() };
    let mut reader = Reader::from_str(s);
    let mut out = Vec::new();
    loop {
        let ev = match reader.read_event() {
            Ok(e) => e,
            Err(_) => break,
        };
        match ev {
            Event::Empty(e) | Event::Start(e) if e.name().as_ref() == b"Relationship" => {
                let id = attr_w_val(&e, b"Id");
                let target = attr_w_val(&e, b"Target");
                if let (Some(id), Some(target)) = (id, target) {
                    out.push((id, target));
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

/// A rels `Target` (relative to `word/`) → package part name (`header1.xml` →
/// `word/header1.xml`). Absolute targets (`/word/header1.xml`) are de-slashed.
fn normalize_target(target: &str) -> String {
    let t = target.trim_start_matches('/');
    if t.starts_with("word/") {
        t.to_string()
    } else {
        format!("word/{t}")
    }
}

fn attr_r_id(e: &quick_xml::events::BytesStart) -> Option<String> {
    attr_w_val(e, b"r:id")
}

/// Import a `w:hdr`/`w:ftr` part into a region. Each `w:p` becomes a paragraph
/// block; `w:t` runs are text; `w:tab`/`w:br` map onto tab/newline; a `PAGE` field
/// (simple `w:fldSimple` or complex `w:instrText`) becomes a page-number field.
pub(super) fn import_region(xml: &[u8], region: Region, doc: &mut DocMut) {
    let Ok(s) = std::str::from_utf8(xml) else { return };
    let mut reader = Reader::from_str(s);
    let mut prev: Option<String> = None;
    let mut cur: Option<String> = None;
    let mut cur_len: usize = 0;
    let mut in_t = false;
    let mut in_instr = false; // inside a complex-field <w:instrText>
    // A complex field whose instruction text contains PAGE; its result runs are
    // skipped and a single placeholder emitted at the field's start.
    let mut field_pending = false;
    let mut field_in_progress = false; // between fldChar begin/separate and end
    let mut field_skip_text = false; // skip the field's result-run text
    // Depth inside a non-self-closing `w:fldSimple` (its cached result-run text is
    // skipped — the placeholder already stands in for the whole field).
    let mut fldsimple_depth = 0usize;
    let mut field_marks: Vec<(String, usize)> = Vec::new();

    let open_block = |doc: &mut DocMut, prev: &mut Option<String>, cur: &mut Option<String>, cur_len: &mut usize| {
        let id = doc.insert_block_in(region, prev.as_deref(), "paragraph");
        *prev = Some(id.clone());
        *cur = Some(id.clone());
        *cur_len = 0;
    };

    loop {
        let ev = match reader.read_event() {
            Ok(e) => e,
            Err(_) => break,
        };
        match ev {
            Event::Start(e) => match e.name().as_ref() {
                b"w:p" => open_block(doc, &mut prev, &mut cur, &mut cur_len),
                b"w:t" => in_t = true,
                b"w:instrText" => in_instr = true,
                b"w:fldSimple" => {
                    // A fldSimple with children: emit the field once at its start,
                    // then skip its cached result-run text until it closes.
                    fldsimple_depth += 1;
                    let instr = attr_w_val(&e, b"w:instr").unwrap_or_default();
                    if is_page_instr(&instr) {
                        if let Some(b) = cur.clone() {
                            let start = cur_len;
                            doc.splice_text_in(region, &b, cur_len, 0, &FIELD_PLACEHOLDER.to_string());
                            cur_len += 1;
                            field_marks.push((b, start));
                        }
                    }
                }
                _ => {}
            },
            Event::Empty(e) => match e.name().as_ref() {
                b"w:tab" => {
                    if let Some(b) = cur.clone() {
                        doc.splice_text_in(region, &b, cur_len, 0, "\t");
                        cur_len += 1;
                    }
                }
                b"w:br" | b"w:cr" => {
                    if let Some(b) = cur.clone() {
                        doc.splice_text_in(region, &b, cur_len, 0, "\n");
                        cur_len += 1;
                    }
                }
                b"w:fldSimple" => {
                    // Simple field: a PAGE instruction → a page-number field.
                    let instr = attr_w_val(&e, b"w:instr").unwrap_or_default();
                    if is_page_instr(&instr) {
                        if let Some(b) = cur.clone() {
                            let start = cur_len;
                            doc.splice_text_in(region, &b, cur_len, 0, &FIELD_PLACEHOLDER.to_string());
                            cur_len += 1;
                            field_marks.push((b, start));
                        }
                    }
                }
                b"w:fldChar" => {
                    match attr_w_val(&e, b"w:fldCharType").as_deref() {
                        Some("begin") => {
                            field_in_progress = true;
                            field_pending = false;
                            field_skip_text = false;
                        }
                        Some("separate") => {
                            // Instruction collected; if it was PAGE, emit the field
                            // now and skip the result-run text until "end".
                            if field_pending {
                                if let Some(b) = cur.clone() {
                                    let start = cur_len;
                                    doc.splice_text_in(region, &b, cur_len, 0, &FIELD_PLACEHOLDER.to_string());
                                    cur_len += 1;
                                    field_marks.push((b, start));
                                }
                                field_skip_text = true;
                            }
                        }
                        Some("end") => {
                            field_in_progress = false;
                            field_pending = false;
                            field_skip_text = false;
                        }
                        _ => {}
                    }
                }
                _ => {}
            },
            Event::Text(t) => {
                let Ok(txt) = t.xml10_content() else { continue };
                if in_instr {
                    if field_in_progress && is_page_instr(&txt) {
                        field_pending = true;
                    }
                } else if in_t && !field_skip_text && fldsimple_depth == 0 {
                    if let Some(b) = cur.clone() {
                        if !txt.is_empty() {
                            doc.splice_text_in(region, &b, cur_len, 0, &txt);
                            cur_len += txt.chars().count();
                        }
                    }
                }
            }
            Event::End(e) => match e.name().as_ref() {
                b"w:t" => in_t = false,
                b"w:instrText" => in_instr = false,
                b"w:fldSimple" => fldsimple_depth = fldsimple_depth.saturating_sub(1),
                b"w:p" => cur = None,
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }

    for (b, start) in field_marks {
        doc.add_mark_in(region, &b, start, start + 1, MARK_FIELD, FIELD_PAGE_NUMBER);
    }
}

/// Whether a field instruction string is a PAGE field (`" PAGE "`, `"PAGE \* …"`).
fn is_page_instr(instr: &str) -> bool {
    instr.split_whitespace().next().map(|w| w.eq_ignore_ascii_case("PAGE")).unwrap_or(false)
}

fn attr_w_val(e: &quick_xml::events::BytesStart, key: &[u8]) -> Option<String> {
    e.attributes().flatten().find(|a| a.key.as_ref() == key).and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
}

fn push_escaped(out: &mut String, c: char) {
    match c {
        '&' => out.push_str("&amp;"),
        '<' => out.push_str("&lt;"),
        '>' => out.push_str("&gt;"),
        _ => out.push(c),
    }
}

/// Whether character `i` of `b` is a page-number field placeholder.
fn page_number_field_at(b: &BlockView, i: usize) -> bool {
    b.marks
        .iter()
        .any(|m| m.name == MARK_FIELD && m.start == i && m.end == i + 1 && m.value.as_str() == Some(FIELD_PAGE_NUMBER))
}

/// Render a region block as `<w:p>` content: plain `w:t` runs, `w:tab`/`w:br` for
/// tabs/breaks, and a `w:fldSimple w:instr=" PAGE "` for a page-number field.
fn render_block(b: &BlockView) -> String {
    let chars: Vec<char> = b.text.chars().collect();
    let mut out = String::from("<w:p>");
    let mut buf = String::new();
    let flush = |out: &mut String, buf: &mut String| {
        if !buf.is_empty() {
            out.push_str(r#"<w:r><w:t xml:space="preserve">"#);
            for c in buf.chars() {
                push_escaped(out, c);
            }
            out.push_str("</w:t></w:r>");
            buf.clear();
        }
    };
    for (i, &c) in chars.iter().enumerate() {
        if c == FIELD_PLACEHOLDER {
            flush(&mut out, &mut buf);
            if page_number_field_at(b, i) {
                // Self-closing so the import sees no cached result-run text (the
                // field's displayed value is recomputed by the renderer).
                out.push_str(r#"<w:fldSimple w:instr=" PAGE "/>"#);
            }
            continue;
        }
        match c {
            '\t' => {
                flush(&mut out, &mut buf);
                out.push_str("<w:r><w:tab/></w:r>");
            }
            '\n' => {
                flush(&mut out, &mut buf);
                out.push_str("<w:r><w:br/></w:r>");
            }
            _ => buf.push(c),
        }
    }
    flush(&mut out, &mut buf);
    out.push_str("</w:p>");
    out
}

/// The full `word/header1.xml` (`w:hdr`) / `word/footer1.xml` (`w:ftr`) bytes for a
/// region's blocks, or `None` when the region is empty.
fn region_part_xml(blocks: &[BlockView], root: &str) -> Option<Vec<u8>> {
    if blocks.is_empty() {
        return None;
    }
    let mut inner = String::new();
    for b in blocks {
        inner.push_str(&render_block(b));
    }
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:{root} xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">{inner}</w:{root}>"#
    );
    Some(xml.into_bytes())
}

/// The exported header/footer parts (`(part_name, bytes)`) for the document, plus
/// whether each exists. Empty regions are omitted.
pub(super) struct ExportedParts {
    pub header: Option<Vec<u8>>,
    pub footer: Option<Vec<u8>>,
}

impl ExportedParts {
    pub fn any(&self) -> bool {
        self.header.is_some() || self.footer.is_some()
    }
}

/// Build the header/footer part bytes from the document's regions.
pub(super) fn export_parts(doc: &DocRef) -> ExportedParts {
    let header = materialize_region(doc.doc, doc.c, Region::Header);
    let footer = materialize_region(doc.doc, doc.c, Region::Footer);
    ExportedParts {
        header: region_part_xml(&header, "hdr"),
        footer: region_part_xml(&footer, "ftr"),
    }
}

/// The `w:headerReference`/`w:footerReference` elements (referencing the fixed rel
/// ids) for a `w:sectPr`, given which parts exist.
pub(super) fn section_references(parts: &ExportedParts) -> String {
    let mut s = String::new();
    if parts.header.is_some() {
        s.push_str(&format!(r#"<w:headerReference w:type="default" r:id="{HEADER_REL_ID}"/>"#));
    }
    if parts.footer.is_some() {
        s.push_str(&format!(r#"<w:footerReference w:type="default" r:id="{FOOTER_REL_ID}"/>"#));
    }
    s
}
