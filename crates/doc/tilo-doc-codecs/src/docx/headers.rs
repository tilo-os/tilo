//! Page header / footer ⇄ OOXML header/footer parts. `WordprocessingML` keeps each page
//! region in its own part (`word/header1.xml`/`word/footer1.xml`, `w:hdr`/`w:ftr`),
//! referenced from the body's `w:sectPr` by relationship id. The canonical model holds one
//! header and one footer, exported as the section's `default` pair; `first`/`even` variants
//! are carried through unedited (see `section`). A page-number field is a
//! `PAGE` field, emitted as `w:fldSimple w:instr=" PAGE "` and parsed from both that
//! simple form and the complex `w:instrText` form.

use super::side_part;
use super::styles::attr_val;
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use std::borrow::Cow;
use tilo_doc::model::{DocMut, DocRef, Region, materialize_region};
use tilo_doc::schema::{FIELD_PAGE_NUMBER, FIELD_PLACEHOLDER, MARK_FIELD};
use tilo_doc::snapshot::BlockView;

/// The relationship ids qdoc's own header/footer are referenced under; the parts they point at
/// are named per export (see `section::plan`).
pub(super) const HEADER_REL_ID: &str = "rIdQdocHdr";
pub(super) const FOOTER_REL_ID: &str = "rIdQdocFtr";

/// Resolve the default header + footer part names via `word/_rels/document.xml.rels`.
/// Reads the body's final `w:sectPr` (the one export keeps); prefers its `w:type="default"`
/// reference, then its first reference of each kind, then — as Word does for a section
/// without one — whatever an earlier section referenced.
pub(super) fn resolve_default_parts(
    document_xml: &[u8],
    rels_xml: Option<&[u8]>,
) -> (Option<String>, Option<String>) {
    let (hdr_rid, ftr_rid) = sectpr_reference_ids(document_xml);
    let rels = rels_xml.map(rel_targets).unwrap_or_default();
    let resolve = |rid: Option<String>| -> Option<String> {
        let rid = rid?;
        let target = rels
            .iter()
            .find(|(id, _)| *id == rid)
            .map(|(_, t)| t.clone())?;
        Some(normalize_target(&target))
    };
    (resolve(hdr_rid), resolve(ftr_rid))
}

/// The header/footer references of one `w:sectPr`: the `default` one and the first one.
#[derive(Default)]
struct SectionRefs {
    default: Option<String>,
    first: Option<String>,
}

impl SectionRefs {
    fn record(&mut self, rid: Option<String>, kind: Option<&str>) {
        if self.first.is_none() {
            self.first.clone_from(&rid);
        }
        if kind.is_none_or(|k| k == "default") {
            self.default = rid;
        }
    }

    fn best(self) -> Option<String> {
        self.default.or(self.first)
    }
}

/// The default (or first) header/footer relationship ids of the last `w:sectPr` that has any.
fn sectpr_reference_ids(document_xml: &[u8]) -> (Option<String>, Option<String>) {
    let Ok(s) = std::str::from_utf8(document_xml) else {
        return (None, None);
    };
    let mut reader = Reader::from_str(s);
    let mut in_sectpr = false;
    let mut hdr = SectionRefs::default();
    let mut ftr = SectionRefs::default();
    let mut found_hdr: Option<String> = None;
    let mut found_ftr: Option<String> = None;
    while let Ok(ev) = reader.read_event() {
        match ev {
            Event::Start(e) if e.name().as_ref() == b"w:sectPr" => {
                in_sectpr = true;
                hdr = SectionRefs::default();
                ftr = SectionRefs::default();
            }
            Event::Empty(e) | Event::Start(e) if in_sectpr => match e.name().as_ref() {
                b"w:headerReference" => {
                    hdr.record(attr_r_id(&e), attr_val(&e, b"w:type").as_deref());
                }
                b"w:footerReference" => {
                    ftr.record(attr_r_id(&e), attr_val(&e, b"w:type").as_deref());
                }
                _ => {}
            },
            Event::End(e) if e.name().as_ref() == b"w:sectPr" => {
                in_sectpr = false;
                if let Some(rid) = std::mem::take(&mut hdr).best() {
                    found_hdr = Some(rid);
                }
                if let Some(rid) = std::mem::take(&mut ftr).best() {
                    found_ftr = Some(rid);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    (found_hdr, found_ftr)
}

/// `(Id, Target)` pairs from a `word/_rels/document.xml.rels`.
fn rel_targets(rels_xml: &[u8]) -> Vec<(String, String)> {
    let Ok(s) = std::str::from_utf8(rels_xml) else {
        return Vec::new();
    };
    let mut reader = Reader::from_str(s);
    let mut out = Vec::new();
    while let Ok(ev) = reader.read_event() {
        match ev {
            Event::Empty(e) | Event::Start(e) if e.name().as_ref() == b"Relationship" => {
                let id = attr_val(&e, b"Id");
                let target = attr_val(&e, b"Target");
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

/// A rels `Target` (relative to `word/`) → package part name; absolute targets are de-slashed.
pub(super) fn normalize_target(target: &str) -> String {
    let t = target.trim_start_matches('/');
    if t.starts_with("word/") {
        t.to_string()
    } else {
        format!("word/{t}")
    }
}

fn attr_r_id(e: &quick_xml::events::BytesStart) -> Option<String> {
    attr_val(e, b"r:id")
}

/// Splice the page-number placeholder char at `cur`'s current position and record its start
/// for the deferred field-mark pass.
fn splice_page_placeholder(
    region: Region,
    doc: &mut DocMut,
    cur: Option<&str>,
    cur_len: &mut usize,
    field_marks: &mut Vec<(String, usize)>,
) {
    if let Some(b) = cur {
        let start = *cur_len;
        doc.splice_text_in(region, b, *cur_len, 0, &FIELD_PLACEHOLDER.to_string());
        *cur_len = cur_len.saturating_add(1);
        field_marks.push((b.to_string(), start));
    }
}

/// If `instr` is a PAGE field instruction, splice its placeholder now (the `w:fldSimple` case:
/// the instruction and its cached result are both on the one element).
fn emit_page_field(
    instr: &str,
    region: Region,
    doc: &mut DocMut,
    cur: Option<&str>,
    cur_len: &mut usize,
    field_marks: &mut Vec<(String, usize)>,
) {
    if is_page_instr(instr) {
        splice_page_placeholder(region, doc, cur, cur_len, field_marks);
    }
}

/// Open a fresh `paragraph` block in `region` and make it current.
fn open_region_block(
    region: Region,
    doc: &mut DocMut,
    prev: &mut Option<String>,
    cur: &mut Option<String>,
    cur_len: &mut usize,
) {
    let id = doc.insert_block_in(region, prev.as_deref(), "paragraph");
    *prev = Some(id.clone());
    *cur = Some(id);
    *cur_len = 0;
}

/// Splice a single char (`w:tab`/`w:br`/`w:cr`) at `cur`'s current position, if any.
fn splice_one_char(
    region: Region,
    doc: &mut DocMut,
    cur: Option<&str>,
    cur_len: &mut usize,
    ch: char,
) {
    if let Some(b) = cur {
        let mut buf = [0u8; 4];
        doc.splice_text_in(region, b, *cur_len, 0, ch.encode_utf8(&mut buf));
        *cur_len = cur_len.saturating_add(1);
    }
}

/// The complex-field (`w:fldChar` begin/separate/end) state machine. A complex field whose
/// instruction text contains PAGE emits a single placeholder at its start and skips its
/// result runs.
#[derive(Default)]
struct ComplexField {
    /// The instruction collected so far is PAGE.
    pending: bool,
    /// Between fldChar begin/separate and end.
    in_progress: bool,
    /// Skip the field's result-run text.
    skip_text: bool,
}

impl ComplexField {
    /// Advance for one `w:fldChar` element; `true` when a PAGE field's placeholder should be
    /// spliced now.
    fn advance(&mut self, e: &quick_xml::events::BytesStart) -> bool {
        match attr_val(e, b"w:fldCharType").as_deref() {
            Some("begin") => {
                self.in_progress = true;
                self.pending = false;
                self.skip_text = false;
            }
            // Instruction collected; if it was PAGE, emit the field now and skip result-run text until "end".
            Some("separate") => {
                if self.pending {
                    self.skip_text = true;
                    return true;
                }
            }
            Some("end") => {
                self.in_progress = false;
                self.pending = false;
                self.skip_text = false;
            }
            _ => {}
        }
        false
    }
}

/// The characters a text event, or an entity or character reference (which
/// quick-xml reports as an event of its own), contributes to the text around it;
/// empty when there are none.
fn character_data<'a>(ev: &'a Event<'_>) -> Cow<'a, str> {
    match ev {
        Event::Text(t) => t.xml10_content().unwrap_or_default(),
        Event::GeneralRef(r) => crate::resolve_general_ref(r)
            .map(String::from)
            .unwrap_or_default()
            .into(),
        _ => Cow::Borrowed(""),
    }
}

/// Import a `w:hdr`/`w:ftr` part into a region: `w:p` → paragraph blocks, `w:t` → text,
/// `w:tab`/`w:br` → tab/newline, a `PAGE` field (simple or complex) → a page-number field.
pub(super) fn import_region(xml: &[u8], region: Region, doc: &mut DocMut) {
    let Ok(s) = std::str::from_utf8(xml) else {
        return;
    };
    let mut reader = Reader::from_str(s);
    let mut prev: Option<String> = None;
    let mut cur: Option<String> = None;
    let mut cur_len: usize = 0;
    let mut in_t = false;
    let mut in_instr = false; // inside a complex-field <w:instrText>
    let mut field = ComplexField::default();
    // Depth inside a non-self-closing `w:fldSimple`; its cached result-run text is skipped.
    let mut fldsimple_depth = 0usize;
    let mut field_marks: Vec<(String, usize)> = Vec::new();

    while let Ok(ev) = reader.read_event() {
        // Some writers spell an empty `w:fldChar` as a start/end pair.
        if let Event::Start(e) | Event::Empty(e) = &ev
            && e.name().as_ref() == b"w:fldChar"
        {
            if field.advance(e) {
                splice_page_placeholder(
                    region,
                    doc,
                    cur.as_deref(),
                    &mut cur_len,
                    &mut field_marks,
                );
            }
            continue;
        }
        match ev {
            Event::Start(e) => match e.name().as_ref() {
                b"w:p" => open_region_block(region, doc, &mut prev, &mut cur, &mut cur_len),
                b"w:t" => in_t = true,
                b"w:instrText" => in_instr = true,
                // Emit the field once at its start, then skip its cached result-run text until it closes.
                b"w:fldSimple" => {
                    fldsimple_depth = fldsimple_depth.saturating_add(1);
                    let instr = attr_val(&e, b"w:instr").unwrap_or_default();
                    let cur = cur.as_deref();
                    emit_page_field(&instr, region, doc, cur, &mut cur_len, &mut field_marks);
                }
                _ => {}
            },
            Event::Empty(e) => match e.name().as_ref() {
                b"w:tab" => splice_one_char(region, doc, cur.as_deref(), &mut cur_len, '\t'),
                b"w:br" | b"w:cr" => {
                    splice_one_char(region, doc, cur.as_deref(), &mut cur_len, '\n');
                }
                b"w:fldSimple" => {
                    let instr = attr_val(&e, b"w:instr").unwrap_or_default();
                    let cur = cur.as_deref();
                    emit_page_field(&instr, region, doc, cur, &mut cur_len, &mut field_marks);
                }
                _ => {}
            },
            ev @ (Event::Text(_) | Event::GeneralRef(_)) => {
                let txt = character_data(&ev);
                if in_instr {
                    if field.in_progress && is_page_instr(&txt) {
                        field.pending = true;
                    }
                } else if in_t
                    && !field.skip_text
                    && fldsimple_depth == 0
                    && let Some(b) = cur.clone()
                    && !txt.is_empty()
                {
                    doc.splice_text_in(region, &b, cur_len, 0, &txt);
                    cur_len = cur_len.saturating_add(txt.chars().count());
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
        doc.add_mark_in(
            region,
            &b,
            start,
            start.saturating_add(1),
            MARK_FIELD,
            FIELD_PAGE_NUMBER,
        );
    }
}

/// Whether a field instruction string is a PAGE field (`" PAGE "`, `"PAGE \* …"`).
fn is_page_instr(instr: &str) -> bool {
    instr
        .split_whitespace()
        .next()
        .is_some_and(|w| w.eq_ignore_ascii_case("PAGE"))
}

/// Whether character `i` of `b` is a page-number field placeholder.
fn page_number_field_at(b: &BlockView, i: usize) -> bool {
    b.marks.iter().any(|m| {
        m.name == MARK_FIELD
            && m.start == i
            && m.end == i.saturating_add(1)
            && m.value.as_str() == Some(FIELD_PAGE_NUMBER)
    })
}

/// Render a region block as `<w:p>`, emitting a `w:fldSimple w:instr=" PAGE "` for a
/// page-number field.
fn render_block(b: &BlockView) -> String {
    let chars: Vec<char> = b.text.chars().collect();
    side_part::render_paragraph(b, |i| {
        (chars.get(i) == Some(&FIELD_PLACEHOLDER)).then(|| {
            if page_number_field_at(b, i) {
                // Self-closing so import sees no cached result-run text (recomputed by the renderer).
                r#"<w:fldSimple w:instr=" PAGE "/>"#
            } else {
                ""
            }
        })
    })
}

/// The full header/footer part bytes for a region's blocks, or `None` when empty.
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

/// The exported header/footer parts, plus whether each exists. Empty regions are omitted.
pub(super) struct ExportedParts {
    pub header: Option<Vec<u8>>,
    pub footer: Option<Vec<u8>>,
}

/// qdoc's header/footer parts, less the regions whose original part is kept (`keep`).
pub(super) fn export_parts(doc: &DocRef, keep: KeptRegions) -> ExportedParts {
    let part = |region: Region, kept: bool, tag: &str| {
        if kept {
            return None;
        }
        region_part_xml(&materialize_region(doc.doc, doc.c, region), tag)
    };
    ExportedParts {
        header: part(Region::Header, keep.header, "hdr"),
        footer: part(Region::Footer, keep.footer, "ftr"),
    }
}

/// Which of the imported default header/footer parts are written back as they are.
#[derive(Clone, Copy, Default)]
pub(super) struct KeptRegions {
    pub header: bool,
    pub footer: bool,
}

/// Whether `region` of `doc` is what importing `original` (a header/footer part) gives: the
/// region was not edited, so the original part stays as it is, with everything the model does
/// not hold (fields other than the page number, pictures, tables, formatting).
pub(super) fn region_unchanged(original: &[u8], region: Region, doc: &DocRef) -> bool {
    let mut scratch = tilo_doc::Document::new();
    import_region(original, region, &mut scratch.as_mut());
    let fresh = scratch.as_ref();
    crate::preserve::same_blocks(
        &materialize_region(doc.doc, doc.c, region),
        &materialize_region(fresh.doc, fresh.c, region),
    )
}
