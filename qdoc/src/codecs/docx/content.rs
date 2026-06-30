//! Parse the `word/document.xml` `w:body` into canonical blocks + marks.
//! Styles (`word/styles.xml`) and numbering (`word/numbering.xml`) are pre-parsed
//! into a [`StyleRegistry`]; this walk resolves paragraph/heading/list/character
//! styles against it. Paragraphs become `paragraph`/`heading`/`list_item` blocks
//! depending on `w:pStyle` and `w:numPr`; runs (`w:r`/`w:rPr`/`w:t`) map onto
//! inline marks; tabs/breaks, alignment, fonts and tables all map onto the model.
//! Unmodeled body-level elements are captured verbatim as fidelity atoms.

use super::container::DocxContainer;
use super::styles::{attr_val, line_spacing_from_spacing, mark_from_rpr_child, map_jc, StyleRegistry};
use crate::document::MarkValue;
use crate::format::CodecError;
use crate::model::DocMut;
use crate::schema::{FIELD_PLACEHOLDER, MARK_COMMENT, MARK_NOTE_REF};

use quick_xml::events::Event;
use quick_xml::reader::Reader;
use std::collections::BTreeMap;

/// EMUs per CSS pixel (914400 EMU/inch ÷ 96 px/inch). Used to convert OOXML
/// `wp:extent` sizes to the model's pixel width/height.
const EMU_PER_PX: f64 = 914400.0 / 96.0;

/// Build a `rId → media bytes` map from `word/_rels/document.xml.rels`, resolving
/// each image relationship's `Target` against the package. So the body parser can
/// turn an `a:blip r:embed="rIdN"` into a stored image blob.
pub(crate) fn build_image_map(container: &DocxContainer) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let Some(rels) = container.get("word/_rels/document.xml.rels") else { return out };
    let Ok(s) = std::str::from_utf8(rels) else { return out };
    let mut reader = Reader::from_str(s);
    loop {
        let ev = match reader.read_event() {
            Ok(e) => e,
            Err(_) => break,
        };
        match ev {
            Event::Empty(e) | Event::Start(e) if e.name().as_ref() == b"Relationship" => {
                let id = attr_val(&e, b"Id");
                let target = attr_val(&e, b"Target");
                if let (Some(id), Some(target)) = (id, target) {
                    // Targets are relative to `word/` (e.g. `media/image1.png`);
                    // absolute (`/word/...`) targets are de-slashed.
                    let t = target.trim_start_matches('/');
                    let part = if t.starts_with("word/") { t.to_string() } else { format!("word/{t}") };
                    if let Some(bytes) = container.get(&part) {
                        out.insert(id, bytes.to_vec());
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

/// In-progress `w:drawing` capture: an embedded image with size + positioning.
#[derive(Default)]
struct DrawingState {
    /// `false` = `wp:inline` (inline), `true` = `wp:anchor` (floating).
    floating: bool,
    /// Display size in px from `wp:extent` (cx/cy in EMU).
    width: Option<i64>,
    height: Option<i64>,
    /// Model wrap (`none`|`left`|`right`|`both`) from the wrap element.
    wrap: Option<String>,
    /// Model anchor (`page` if positioned relative to the page, else `paragraph`).
    page_relative: bool,
    /// The `r:embed` rel id of the `a:blip` referencing the media part.
    rel: Option<String>,
}

/// Open a fresh top-level block anchored after the previous one.
fn start_block(doc: &mut DocMut, prev: &mut Option<String>, cur: &mut Option<String>, cur_len: &mut usize, kind: &str) -> String {
    let id = doc.insert_block(prev.as_deref(), kind);
    *prev = Some(id.clone());
    *cur = Some(id.clone());
    *cur_len = 0;
    id
}

/// Append literal text (tab / break) to the current block.
fn append(doc: &mut DocMut, cur: &Option<String>, cur_len: &mut usize, txt: &str) {
    if let Some(b) = cur {
        if !txt.is_empty() {
            doc.splice_text(b, *cur_len, 0, txt);
            *cur_len += txt.chars().count();
        }
    }
}

pub(crate) fn parse_body(
    document_xml: &[u8],
    reg: &StyleRegistry,
    foot_map: &BTreeMap<String, String>,
    end_map: &BTreeMap<String, String>,
    comment_map: &BTreeMap<String, String>,
    image_map: &BTreeMap<String, Vec<u8>>,
    doc: &mut DocMut,
) -> Result<(), CodecError> {
    let s = std::str::from_utf8(document_xml).map_err(|e| CodecError::Parse(e.to_string()))?;
    let mut reader = Reader::from_str(s);

    let mut in_body = false;
    let mut prev: Option<String> = None;
    let mut cur: Option<String> = None;
    let mut cur_len: usize = 0;

    // Paragraph-property scratch, gathered from `w:pPr` then applied at its close.
    let mut in_ppr = false;
    let mut in_numpr = false;
    let mut p_style: Option<String> = None;
    let mut p_align: Option<String> = None;
    let mut p_numid: Option<String> = None;
    let mut p_line_spacing: Option<f64> = None;

    // Run state. Marks are collected for the run's `w:rPr` and applied over the
    // run's text range at `</w:r>` (so later text doesn't expand the mark).
    let mut in_run = false;
    let mut in_rpr = false;
    let mut in_t = false;
    let mut run_start: usize = 0;
    let mut run_marks: Vec<(String, MarkValue)> = Vec::new();

    // (block, start, end, name, value), applied after all text is present.
    let mut pending_marks: Vec<(String, usize, usize, String, MarkValue)> = Vec::new();
    // Paragraph-style inline marks for the current block, applied over [0, len].
    let mut cur_para_marks: Vec<(String, MarkValue)> = Vec::new();
    // (block, char index, note id) reference markers, applied after all text is in.
    let mut note_refs: Vec<(String, usize, String)> = Vec::new();
    // Open comment ranges keyed by docx id: `docx_id → (comment id, block, start char
    // index)`, awaiting their matching `w:commentRangeEnd`.
    let mut open_comments: BTreeMap<String, (String, String, usize)> = BTreeMap::new();
    // (block, start, end, comment id) anchor marks, applied after all text is in.
    let mut comment_ranges: Vec<(String, usize, usize, String)> = Vec::new();

    // In-progress tracked change `<w:ins>`/`<w:del>` wrapper: its qdoc change id, kind,
    // anchor block, and the char index at which the changed range begins. The runs
    // inside splice text normally; the `ins`/`del` mark is applied at the close. Deleted
    // runs carry `<w:delText>` instead of `<w:t>` (treated identically on import).
    let mut change: Option<(String, crate::snapshot::ChangeKind, Option<String>, usize)> = None;
    // (block, start, end, kind, change id) anchor marks, applied after all text is in.
    let mut change_ranges: Vec<(String, usize, usize, crate::snapshot::ChangeKind, String)> = Vec::new();

    // Table state: cells become consecutive `cell` blocks tagged table/row/col.
    let mut table_id: Option<String> = None;
    let mut trow: usize = 0;
    let mut tcol: usize = 0;
    let mut table_seq: usize = 0;

    let mut capture: Option<(usize, i32)> = None; // (start byte, open depth) of a fidelity atom

    // In-progress `w:drawing` (image). Collected as we descend its children; the
    // image becomes its own `image` block at `</w:drawing>` (replacing the enclosing
    // empty paragraph, mirroring how the ODT codec lifts an image-only paragraph).
    let mut drawing: Option<DrawingState> = None;

    macro_rules! flush_para_marks {
        () => {
            if let Some(b) = cur.as_ref() {
                for (name, value) in cur_para_marks.drain(..) {
                    pending_marks.push((b.clone(), 0, cur_len, name, value));
                }
            }
            cur_para_marks.clear();
        };
    }

    // Finalise the just-parsed paragraph's properties onto its (already opened)
    // block: pick the kind (list_item / heading / paragraph), set attrs, and stash
    // the whole-block paragraph-style marks for plain paragraphs.
    macro_rules! finalize_ppr {
        () => {
            if let Some(id) = cur.clone() {
                // Line spacing rides as a direct `line_spacing` Float attr regardless
                // of the block kind (paragraph/heading/list_item). A direct `w:spacing`
                // wins; otherwise inherit it from the paragraph style.
                let line_spacing = p_line_spacing.or_else(|| p_style.as_deref().and_then(|s| reg.para_line_spacing(s)));
                if let Some(ls) = line_spacing {
                    doc.set_attr(&id, "line_spacing", ls);
                }
                let level = p_style.as_deref().and_then(|s| reg.heading_level(s));
                if let Some(numid) = p_numid.clone() {
                    doc.set_kind(&id, "list_item");
                    let ordered = reg.num_ordered.get(&numid).copied().unwrap_or(false);
                    doc.set_attr(&id, "ordered", ordered);
                    // Numbering fidelity (ordered lists only): preserve the format +
                    // start so the list re-exports with its numbering intact. A plain
                    // decimal-from-1 list adds no extra attrs (minimal cross-codec
                    // attr set).
                    if ordered {
                        if let Some(def) = reg.num_def.get(&numid) {
                            if let Some(f) = def.num_format {
                                if f != "decimal" {
                                    doc.set_attr(&id, "num_format", f);
                                }
                            }
                            if let Some(s) = def.num_start {
                                if s != 1 {
                                    doc.set_attr(&id, "num_start", s);
                                    // OOXML numbering restarts by default; a non-1
                                    // start implies a restart at that value.
                                    doc.set_attr(&id, "num_restart", true);
                                }
                            }
                        }
                    }
                    // Direct `w:jc` wins; otherwise inherit alignment from the style.
                    let align = p_align.clone().or_else(|| p_style.as_deref().and_then(|s| reg.para_align(s)));
                    if let Some(a) = align {
                        doc.set_attr(&id, "align", a.as_str());
                    }
                } else if let Some(l) = level {
                    doc.set_kind(&id, "heading");
                    doc.set_attr(&id, "level", l);
                    let align = p_align.clone().or_else(|| p_style.as_deref().and_then(|s| reg.para_align(s)));
                    if let Some(a) = align {
                        doc.set_attr(&id, "align", a.as_str());
                    }
                } else {
                    // A plain paragraph. A *named* (non-heading, non-list) paragraph
                    // style → a real CRDT-native style: lift its definition into the
                    // styles container (keyed by the `w:styleId`) and point the block
                    // at it via an explicit `style` attr, so the editor resolves the
                    // real named style. The style now supplies the block's alignment +
                    // formatting, so those are not *also* lifted onto the block (keeps
                    // the round-trip idempotent). A *direct* `w:jc` override still
                    // rides on the block as an `align` attr. Built-in heading/list
                    // mapping is handled above and untouched.
                    let lifted = p_style
                        .as_deref()
                        .filter(|s| reg.is_named_para(s))
                        .and_then(|s| reg.style_props(s).map(|props| (s.to_string(), props)));
                    if let Some((sid, props)) = lifted {
                        doc.set_style(&sid, &props);
                        doc.set_attr(&id, "style", sid.as_str());
                        if let Some(a) = p_align.clone() {
                            doc.set_attr(&id, "align", a.as_str());
                        }
                    } else {
                        let align = p_align.clone().or_else(|| p_style.as_deref().and_then(|s| reg.para_align(s)));
                        if let Some(a) = align {
                            doc.set_attr(&id, "align", a.as_str());
                        }
                        if let Some(st) = p_style.clone() {
                            cur_para_marks = reg.para_text_marks(&st);
                        }
                    }
                }
            }
        };
    }

    loop {
        let pos_before = reader.buffer_position() as usize;
        let ev = reader.read_event().map_err(|e| CodecError::Parse(format!("xml: {e}")))?;

        if let Some((start, depth)) = capture.as_mut() {
            match &ev {
                Event::Start(_) => *depth += 1,
                Event::End(_) => {
                    *depth -= 1;
                    if *depth == 0 {
                        let raw = s[*start..reader.buffer_position() as usize].to_string();
                        let id = start_block(doc, &mut prev, &mut cur, &mut cur_len, "fidelity");
                        doc.set_attr(&id, "xml", raw.as_str());
                        cur = None;
                        capture = None;
                    }
                }
                _ => {}
            }
            continue;
        }

        // Inside a `w:drawing`: collect inline-vs-anchor, extent, wrap, and the blip
        // rel, then materialise an `image` block on `</w:drawing>`.
        if let Some(dr) = drawing.as_mut() {
            match &ev {
                Event::Start(e) | Event::Empty(e) => match e.name().as_ref() {
                    b"wp:inline" => dr.floating = false,
                    b"wp:anchor" => dr.floating = true,
                    b"wp:extent" => {
                        if let Some(cx) = attr_val(e, b"cx").and_then(|v| v.parse::<f64>().ok()) {
                            dr.width = Some((cx / EMU_PER_PX).round() as i64);
                        }
                        if let Some(cy) = attr_val(e, b"cy").and_then(|v| v.parse::<f64>().ok()) {
                            dr.height = Some((cy / EMU_PER_PX).round() as i64);
                        }
                    }
                    // Wrap modes. `wrapNone`/`wrapTopAndBottom` don't flow text beside
                    // the image → `none`; `wrapSquare`/`wrapTight`/`wrapThrough` use the
                    // `wrapText` side (`left`/`right`/`bothSides`→both, default both).
                    b"wp:wrapNone" | b"wp:wrapTopAndBottom" => dr.wrap = Some("none".to_string()),
                    b"wp:wrapSquare" | b"wp:wrapTight" | b"wp:wrapThrough" => {
                        let side = attr_val(e, b"wrapText");
                        dr.wrap = Some(match side.as_deref() {
                            Some("left") => "left",
                            Some("right") => "right",
                            _ => "both", // "bothSides" | "largest" | absent
                        }
                        .to_string());
                    }
                    // Page-relative horizontal/vertical positioning marks a page anchor.
                    b"wp:positionH" | b"wp:positionV" => {
                        if attr_val(e, b"relativeFrom").as_deref() == Some("page") {
                            dr.page_relative = true;
                        }
                    }
                    b"a:blip" => {
                        if dr.rel.is_none() {
                            dr.rel = attr_val(e, b"r:embed").or_else(|| attr_val(e, b"r:link"));
                        }
                    }
                    _ => {}
                },
                Event::End(e) if e.name().as_ref() == b"w:drawing" => {
                    let dr = drawing.take().unwrap();
                    let bytes = dr.rel.as_deref().and_then(|r| image_map.get(r));
                    if let Some(bytes) = bytes {
                        let hash = doc.put_blob(bytes);
                        // Reuse the enclosing (image-only) paragraph if it holds no text,
                        // else open a fresh block. Either way it becomes an `image`.
                        let id = if let (Some(c), 0) = (cur.clone(), cur_len) {
                            doc.set_kind(&c, "image");
                            c
                        } else {
                            start_block(doc, &mut prev, &mut cur, &mut cur_len, "image")
                        };
                        doc.set_attr(&id, "blob", hash.as_str());
                        let anchor = if !dr.floating {
                            "inline"
                        } else if dr.page_relative {
                            "page"
                        } else {
                            "paragraph"
                        };
                        doc.set_attr(&id, "anchor", anchor);
                        if let Some(w) = dr.width {
                            if w > 0 {
                                doc.set_attr(&id, "width", w);
                            }
                        }
                        if let Some(h) = dr.height {
                            if h > 0 {
                                doc.set_attr(&id, "height", h);
                            }
                        }
                        if let Some(w) = dr.wrap {
                            doc.set_attr(&id, "wrap", w.as_str());
                        }
                        // An image block isn't a text container; close it.
                        prev = Some(id);
                        cur = None;
                    }
                    in_run = false;
                }
                _ => {}
            }
            continue;
        }

        match ev {
            Event::Start(e) => match e.name().as_ref() {
                b"w:body" => in_body = true,
                // A paragraph (outside a table) opens a block; its kind is refined
                // once `w:pPr` closes.
                b"w:p" if in_body && table_id.is_none() => {
                    start_block(doc, &mut prev, &mut cur, &mut cur_len, "paragraph");
                    p_style = None;
                    p_align = None;
                    p_numid = None;
                    p_line_spacing = None;
                    cur_para_marks.clear();
                }
                b"w:pPr" if table_id.is_none() => in_ppr = true,
                b"w:numPr" if in_ppr => in_numpr = true,
                // A tracked-change wrapper around the runs it covers. Create the change
                // record now (author/date from attrs) and remember the start char index;
                // the runs inside splice text, and the `ins`/`del` mark is applied over
                // `[start, cur_len)` at the close.
                b"w:ins" | b"w:del" if in_body && cur.is_some() => {
                    let kind = if e.name().as_ref() == b"w:ins" {
                        crate::snapshot::ChangeKind::Ins
                    } else {
                        crate::snapshot::ChangeKind::Del
                    };
                    let author = attr_val(&e, b"w:author").unwrap_or_default();
                    let date = attr_val(&e, b"w:date").unwrap_or_default();
                    let cid = doc.insert_change(kind, &author, &date);
                    change = Some((cid, kind, cur.clone(), cur_len));
                }
                b"w:r" if in_body => {
                    in_run = true;
                    run_start = cur_len;
                    run_marks.clear();
                }
                b"w:rPr" if in_run => in_rpr = true,
                // An embedded image: begin collecting the drawing's size/positioning/
                // blip until `</w:drawing>`. (`in_run` is always true here in practice.)
                b"w:drawing" if in_body => {
                    drawing = Some(DrawingState::default());
                }
                b"w:t" if in_run => in_t = true,
                // Deleted text inside a `<w:del>` run uses `<w:delText>`; treat it like
                // `<w:t>` so the retained-but-struck content lands in the model.
                b"w:delText" if in_run => in_t = true,
                b"w:tbl" if in_body => {
                    table_seq += 1;
                    table_id = Some(format!("t{table_seq}"));
                    trow = 0;
                }
                b"w:tr" if table_id.is_some() => tcol = 0,
                b"w:tc" if table_id.is_some() => {
                    if let Some(tid) = table_id.clone() {
                        let id = start_block(doc, &mut prev, &mut cur, &mut cur_len, "cell");
                        doc.set_attr(&id, "table", tid.as_str());
                        doc.set_attr(&id, "row", trow as i64);
                        doc.set_attr(&id, "col", tcol as i64);
                    }
                }
                // Unknown block-level element → capture verbatim as a fidelity atom.
                _ if in_body && cur.is_none() && table_id.is_none() => {
                    capture = Some((pos_before, 1));
                }
                _ => {}
            },
            Event::Empty(e) => match e.name().as_ref() {
                b"w:p" if in_body && table_id.is_none() => {
                    start_block(doc, &mut prev, &mut cur, &mut cur_len, "paragraph");
                    cur = None;
                }
                b"w:pStyle" if in_ppr => p_style = attr_val(&e, b"w:val"),
                b"w:jc" if in_ppr => p_align = attr_val(&e, b"w:val").map(|v| map_jc(&v)),
                // Paragraph line spacing: `<w:spacing w:line w:lineRule="auto">`.
                b"w:spacing" if in_ppr => {
                    if let Some(ls) = line_spacing_from_spacing(&e) {
                        p_line_spacing = Some(ls);
                    }
                }
                b"w:numId" if in_numpr => p_numid = attr_val(&e, b"w:val"),
                b"w:tab" if in_run => append(doc, &cur, &mut cur_len, "\t"),
                b"w:br" | b"w:cr" if in_run => append(doc, &cur, &mut cur_len, "\n"),
                b"w:footnoteReference" | b"w:endnoteReference" if in_run => {
                    // A note reference: resolve its docx id to a qdoc note id (via the
                    // pre-parsed parts), splice a placeholder, and defer the `note-ref`
                    // marker until all text is present.
                    let is_foot = e.name().as_ref() == b"w:footnoteReference";
                    let map = if is_foot { foot_map } else { end_map };
                    if let Some(docx_id) = attr_val(&e, b"w:id") {
                        if let Some(note_id) = map.get(&docx_id) {
                            if let Some(b) = cur.clone() {
                                let idx = cur_len;
                                append(doc, &cur, &mut cur_len, &FIELD_PLACEHOLDER.to_string());
                                note_refs.push((b, idx, note_id.clone()));
                            }
                        }
                    }
                }
                b"w:commentRangeStart" if in_body && cur.is_some() => {
                    // The start of a commented range: remember the current char index;
                    // the matching `w:commentRangeEnd` records the end. The range marks
                    // sit at body level (between runs), so `cur_len` is the boundary.
                    if let Some(docx_id) = attr_val(&e, b"w:id") {
                        if let Some(cid) = comment_map.get(&docx_id) {
                            if let Some(b) = cur.clone() {
                                open_comments.insert(docx_id, (cid.clone(), b, cur_len));
                            }
                        }
                    }
                }
                b"w:commentRangeEnd" if in_body => {
                    // Closes a commented range: anchor a `comment` mark over
                    // `[start, cur_len)` for the matching docx id.
                    if let Some(docx_id) = attr_val(&e, b"w:id") {
                        if let Some((cid, b, start)) = open_comments.remove(&docx_id) {
                            comment_ranges.push((b, start, cur_len, cid));
                        }
                    }
                }
                // `w:commentReference` (the citation marker run child) carries no
                // range info — the range is defined by start/end — so it is ignored.
                _ if in_rpr => {
                    if let Some((k, v)) = mark_from_rpr_child(&e) {
                        run_marks.retain(|(k2, _)| k2 != &k);
                        run_marks.push((k, v));
                    } else if e.name().as_ref() == b"w:rStyle" {
                        if let Some(name) = attr_val(&e, b"w:val") {
                            for (k, v) in reg.text_marks(&name) {
                                run_marks.retain(|(k2, _)| k2 != &k);
                                run_marks.push((k, v));
                            }
                        }
                    }
                }
                // Unknown block-level empty element → fidelity atom.
                _ if in_body && cur.is_none() && table_id.is_none() => {
                    let raw = s[pos_before..reader.buffer_position() as usize].to_string();
                    let id = start_block(doc, &mut prev, &mut cur, &mut cur_len, "fidelity");
                    doc.set_attr(&id, "xml", raw.as_str());
                    cur = None;
                }
                _ => {}
            },
            Event::End(e) => match e.name().as_ref() {
                b"w:body" => in_body = false,
                b"w:pPr" if table_id.is_none() => {
                    in_ppr = false;
                    finalize_ppr!();
                }
                b"w:numPr" => in_numpr = false,
                b"w:rPr" => in_rpr = false,
                b"w:t" => in_t = false,
                b"w:delText" => in_t = false,
                b"w:ins" | b"w:del" => {
                    // Close the tracked-change wrapper: anchor an `ins`/`del` mark over
                    // `[start, cur_len)` for the change (applied after all text is in).
                    if let Some((cid, kind, block, start)) = change.take() {
                        if let Some(b) = block {
                            change_ranges.push((b, start, cur_len, kind, cid));
                        }
                    }
                }
                b"w:r" => {
                    in_run = false;
                    if let Some(b) = cur.as_ref() {
                        for (name, value) in run_marks.drain(..) {
                            pending_marks.push((b.clone(), run_start, cur_len, name, value));
                        }
                    }
                    run_marks.clear();
                }
                b"w:p" if table_id.is_none() => {
                    flush_para_marks!();
                    cur = None;
                }
                b"w:tc" if table_id.is_some() => {
                    cur = None;
                    tcol += 1;
                }
                b"w:tr" if table_id.is_some() => trow += 1,
                b"w:tbl" => {
                    table_id = None;
                    cur = None;
                }
                _ => {}
            },
            Event::Text(t) => {
                if in_t {
                    if let Some(b) = cur.as_ref() {
                        let txt = t.unescape().map_err(|e| CodecError::Parse(format!("text: {e}")))?;
                        if !txt.is_empty() {
                            doc.splice_text(b, cur_len, 0, &txt);
                            cur_len += txt.chars().count();
                        }
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }

    // Apply marks now that all text is present (exact ranges, no expansion).
    for (b, start, end, name, value) in pending_marks {
        if start >= end {
            continue;
        }
        match value {
            MarkValue::Bool(v) => doc.add_mark(&b, start, end, &name, v),
            MarkValue::Int(n) => doc.add_mark(&b, start, end, &name, n),
            MarkValue::Str(s) => doc.add_mark(&b, start, end, &name, s.as_str()),
        }
    }
    // Note-reference markers over the placeholder spliced at each reference element.
    for (b, idx, nid) in note_refs {
        doc.add_mark(&b, idx, idx + 1, MARK_NOTE_REF, nid.as_str());
    }
    // A comment whose `w:commentRangeEnd` never arrived still anchors at its start,
    // extending to the block's end so it has a non-empty range when content follows.
    for (_docx_id, (cid, b, start)) in std::mem::take(&mut open_comments) {
        let end = doc_block_len(doc, &b).max(start);
        comment_ranges.push((b, start, end, cid));
    }
    // Comment anchor marks: cover each commented range with a `comment` mark valued
    // with the comment id. A zero-width range can't carry a mark, so widen it to one
    // character when the block has text after the anchor.
    for (b, start, mut end, cid) in comment_ranges {
        if end <= start {
            let blen = doc_block_len(doc, &b);
            end = (start + 1).min(blen);
        }
        if end > start {
            doc.add_mark(&b, start, end, MARK_COMMENT, cid.as_str());
        }
    }
    // Tracked-change anchor marks: cover each changed range with the `ins`/`del` mark
    // matching the change's kind, valued with the change id.
    for (b, start, end, kind, cid) in change_ranges {
        if end > start {
            doc.add_change_mark(kind, &b, start, end, &cid);
        }
    }
    Ok(())
}

/// The character length of a body block's text (0 if absent).
fn doc_block_len(doc: &DocMut, block_id: &str) -> usize {
    crate::model::block_text(doc.doc, doc.c, block_id).map(|t| t.chars().count()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::super::container::{minimal_docx, minimal_docx_numbered, minimal_docx_styled};
    use super::super::DocxCodec;
    use crate::{AttrVal, Document, FormatProvider};

    #[test]
    fn imports_headings_and_paragraphs() {
        let body = concat!(
            r#"<w:p><w:pPr><w:pStyle w:val="Heading2"/></w:pPr><w:r><w:t>Title</w:t></w:r></w:p>"#,
            r#"<w:p><w:r><w:t>Hello</w:t></w:r></w:p>"#,
        );
        let docx = minimal_docx(body);
        let mut d = Document::new();
        DocxCodec.import(&docx, &mut d.as_mut()).unwrap();
        let snap = d.materialize();

        assert_eq!(snap.blocks[0].kind, "heading");
        assert_eq!(snap.blocks[0].text, "Title");
        assert_eq!(snap.blocks[0].attrs.get("level"), Some(&AttrVal::Int(2)));
        assert_eq!(snap.blocks[1].kind, "paragraph");
        assert_eq!(snap.blocks[1].text, "Hello");
    }

    #[test]
    fn imports_heading_via_named_style_with_style_alignment() {
        // A real styles.xml: a custom paragraph style derived from Heading1 that
        // also centres its text. Heading detection + alignment both come from the
        // resolved style, not from anything on the paragraph itself.
        let styles = concat!(
            r#"<w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:pPr><w:outlineLvl w:val="0"/><w:jc w:val="center"/></w:pPr></w:style>"#,
            r#"<w:style w:type="paragraph" w:styleId="MyTitle"><w:basedOn w:val="Heading1"/></w:style>"#,
        );
        let body = r#"<w:p><w:pPr><w:pStyle w:val="MyTitle"/></w:pPr><w:r><w:t>My Title</w:t></w:r></w:p>"#;
        let docx = minimal_docx_styled(styles, body);
        let mut d = Document::new();
        DocxCodec.import(&docx, &mut d.as_mut()).unwrap();
        let b = &d.materialize().blocks[0];
        assert_eq!(b.kind, "heading", "named paragraph style → heading");
        assert_eq!(b.text, "My Title");
        assert_eq!(b.attrs.get("level"), Some(&AttrVal::Int(1)), "level inherited via basedOn");
        assert_eq!(b.attrs.get("align"), Some(&AttrVal::Str("center".into())), "alignment from the style");
    }

    #[test]
    fn imports_run_props_as_marks() {
        use crate::document::MarkValue;
        let body = concat!(
            r#"<w:p>"#,
            r#"<w:r><w:t xml:space="preserve">a </w:t></w:r>"#,
            r#"<w:r><w:rPr><w:b/></w:rPr><w:t>bold</w:t></w:r>"#,
            r#"<w:r><w:t xml:space="preserve"> and </w:t></w:r>"#,
            r#"<w:r><w:rPr><w:i/><w:u w:val="single"/><w:strike/></w:rPr><w:t>more</w:t></w:r>"#,
            r#"</w:p>"#,
        );
        let docx = minimal_docx(body);
        let mut d = Document::new();
        DocxCodec.import(&docx, &mut d.as_mut()).unwrap();
        let p = &d.materialize().blocks[0];
        assert_eq!(p.text, "a bold and more");
        assert!(p.marks.iter().any(|m| m.name == "strong" && m.start == 2 && m.end == 6), "{:?}", p.marks);
        assert!(p.marks.iter().any(|m| m.name == "em" && m.start == 11 && m.end == 15), "{:?}", p.marks);
        assert!(p.marks.iter().any(|m| m.name == "underline" && m.start == 11 && m.end == 15), "{:?}", p.marks);
        assert!(p.marks.iter().any(|m| m.name == "strike" && m.start == 11 && m.end == 15), "{:?}", p.marks);
        let _ = MarkValue::Bool(true);
    }

    #[test]
    fn imports_align_font_size_and_family() {
        use crate::document::MarkValue;
        let body = concat!(
            r#"<w:p><w:pPr><w:jc w:val="center"/></w:pPr>"#,
            r#"<w:r><w:rPr><w:sz w:val="40"/><w:rFonts w:ascii="Times New Roman"/></w:rPr><w:t>big</w:t></w:r>"#,
            r#"</w:p>"#,
        );
        let docx = minimal_docx(body);
        let mut d = Document::new();
        DocxCodec.import(&docx, &mut d.as_mut()).unwrap();
        let b = &d.materialize().blocks[0];
        assert_eq!(b.text, "big");
        assert_eq!(b.attrs.get("align"), Some(&AttrVal::Str("center".into())), "{:?}", b.attrs);
        assert!(b.marks.iter().any(|m| m.name == "fontSize" && m.value == MarkValue::Int(20)), "{:?}", b.marks);
        assert!(b.marks.iter().any(|m| m.name == "fontFamily" && m.value == MarkValue::Str("serif".into())), "{:?}", b.marks);
    }

    #[test]
    fn imports_named_character_style_and_whitespace() {
        let styles = r#"<w:style w:type="character" w:styleId="Strong"><w:rPr><w:b/></w:rPr></w:style>"#;
        let body = concat!(
            r#"<w:p><w:r><w:t>x</w:t></w:r>"#,
            r#"<w:r><w:rPr><w:rStyle w:val="Strong"/></w:rPr><w:t>y</w:t></w:r></w:p>"#,
            r#"<w:p><w:r><w:t>one</w:t><w:br/><w:t>two</w:t><w:tab/><w:t>three</w:t></w:r></w:p>"#,
        );
        let docx = minimal_docx_styled(styles, body);
        let mut d = Document::new();
        DocxCodec.import(&docx, &mut d.as_mut()).unwrap();
        let snap = d.materialize();
        let p0 = &snap.blocks[0];
        assert_eq!(p0.text, "xy");
        assert!(p0.marks.iter().any(|m| m.name == "strong" && m.start == 1 && m.end == 2), "named char style → mark: {:?}", p0.marks);
        let p1 = &snap.blocks[1];
        assert!(p1.text.contains('\n'), "w:br → newline: {:?}", p1.text);
        assert!(p1.text.contains('\t'), "w:tab → tab: {:?}", p1.text);
        assert_eq!(p1.text, "one\ntwo\tthree");
    }

    #[test]
    fn imports_numbered_and_bulleted_lists() {
        let numbering = concat!(
            r#"<w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:numFmt w:val="bullet"/></w:lvl></w:abstractNum>"#,
            r#"<w:abstractNum w:abstractNumId="1"><w:lvl w:ilvl="0"><w:numFmt w:val="decimal"/></w:lvl></w:abstractNum>"#,
            r#"<w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>"#,
            r#"<w:num w:numId="2"><w:abstractNumId w:val="1"/></w:num>"#,
        );
        let body = concat!(
            r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="2"/></w:numPr></w:pPr><w:r><w:t>first</w:t></w:r></w:p>"#,
            r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>bee</w:t></w:r></w:p>"#,
        );
        let docx = minimal_docx_numbered("", numbering, body);
        let mut d = Document::new();
        DocxCodec.import(&docx, &mut d.as_mut()).unwrap();
        let snap = d.materialize();
        let items: Vec<_> = snap.blocks.iter().filter(|b| b.kind == "list_item").collect();
        assert_eq!(items.len(), 2, "{snap:?}");
        assert_eq!(items[0].text, "first");
        assert_eq!(items[0].attrs.get("ordered"), Some(&AttrVal::Bool(true)), "decimal numId → ordered");
        assert_eq!(items[1].attrs.get("ordered"), Some(&AttrVal::Bool(false)), "bullet numId → unordered");
    }

    #[test]
    fn imports_table_as_editable_cells() {
        let body = concat!(
            r#"<w:tbl>"#,
            r#"<w:tr><w:tc><w:p><w:r><w:t>a</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>b</w:t></w:r></w:p></w:tc></w:tr>"#,
            r#"<w:tr><w:tc><w:p><w:r><w:t>c</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>d</w:t></w:r></w:p></w:tc></w:tr>"#,
            r#"</w:tbl>"#,
        );
        let docx = minimal_docx(body);
        let mut d = Document::new();
        DocxCodec.import(&docx, &mut d.as_mut()).unwrap();
        let snap = d.materialize();
        let cells: Vec<_> = snap.blocks.iter().filter(|b| b.kind == "cell").collect();
        assert_eq!(cells.len(), 4, "2x2 cells: {snap:?}");
        assert_eq!(cells.iter().map(|c| c.text.clone()).collect::<Vec<_>>(), vec!["a", "b", "c", "d"]);
        assert_eq!(cells[3].attrs.get("row"), Some(&AttrVal::Int(1)));
        assert_eq!(cells[3].attrs.get("col"), Some(&AttrVal::Int(1)));
    }

    #[test]
    fn captures_unmodeled_body_element_as_fidelity_atom() {
        let body = concat!(
            r#"<w:p><w:r><w:t>before</w:t></w:r></w:p>"#,
            r#"<w:sdt><w:sdtContent><w:r><w:t>boxed</w:t></w:r></w:sdtContent></w:sdt>"#,
            r#"<w:p><w:r><w:t>after</w:t></w:r></w:p>"#,
        );
        let docx = minimal_docx(body);
        let mut d = Document::new();
        DocxCodec.import(&docx, &mut d.as_mut()).unwrap();
        let snap = d.materialize();
        assert_eq!(snap.blocks.len(), 3, "before / fidelity / after: {snap:?}");
        assert_eq!(snap.blocks[0].text, "before");
        assert_eq!(snap.blocks[1].kind, "fidelity");
        assert_eq!(snap.blocks[2].text, "after");
        let raw = match snap.blocks[1].attrs.get("xml") {
            Some(AttrVal::Str(x)) => x.clone(),
            other => panic!("fidelity atom missing xml attr: {other:?}"),
        };
        assert!(raw.starts_with("<w:sdt") && raw.contains("boxed") && raw.ends_with("</w:sdt>"), "captured raw: {raw}");
    }

    #[test]
    fn imports_named_paragraph_style_into_container() {
        // A custom (non-heading, non-list) paragraph style carrying formatting →
        // a CRDT-native style in the styles container, with the block pointing at
        // it via its `w:styleId` rather than the formatting being baked onto the
        // block. Mirrors the `.odt` named-style import.
        let styles = concat!(
            r##"<w:style w:type="paragraph" w:styleId="Fancy"><w:name w:val="Fancy"/>"##,
            r##"<w:pPr><w:jc w:val="center"/><w:spacing w:after="180"/><w:ind w:left="360"/></w:pPr>"##,
            r##"<w:rPr><w:rFonts w:ascii="Times New Roman"/><w:b/><w:i/><w:color w:val="FF0000"/><w:sz w:val="40"/></w:rPr></w:style>"##,
        );
        let body = r#"<w:p><w:pPr><w:pStyle w:val="Fancy"/></w:pPr><w:r><w:t>hi</w:t></w:r></w:p>"#;
        let docx = minimal_docx_styled(styles, body);
        let mut d = Document::new();
        DocxCodec.import(&docx, &mut d.as_mut()).unwrap();
        let snap = d.materialize();
        let b = &snap.blocks[0];
        assert_eq!(b.kind, "paragraph");
        assert_eq!(b.text, "hi");
        assert_eq!(b.attrs.get("style"), Some(&AttrVal::Str("Fancy".into())), "block points at the styleId: {:?}", b.attrs);
        // The formatting lives in the style, not directly on the block.
        assert_eq!(b.attrs.get("align"), None, "alignment comes from the style, not a direct attr: {:?}", b.attrs);
        assert!(b.marks.is_empty(), "style formatting not lifted as inline marks: {:?}", b.marks);

        let st = snap.styles.get("Fancy").expect("style lifted into container");
        assert_eq!(st.get("name"), Some(&AttrVal::Str("Fancy".into())));
        assert_eq!(st.get("bold"), Some(&AttrVal::Bool(true)));
        assert_eq!(st.get("italic"), Some(&AttrVal::Bool(true)));
        assert_eq!(st.get("color"), Some(&AttrVal::Str("#ff0000".into())), "w:color FF0000 → #ff0000");
        assert_eq!(st.get("align"), Some(&AttrVal::Str("center".into())));
        assert_eq!(st.get("size"), Some(&AttrVal::Float(20.0)), "w:sz 40 half-points → 20pt");
        assert_eq!(st.get("font"), Some(&AttrVal::Str("serif".into())), "Times New Roman → serif");
        assert_eq!(st.get("space_after"), Some(&AttrVal::Float(12.0)), "180 twips → 12px");
        assert_eq!(st.get("indent"), Some(&AttrVal::Float(24.0)), "360 twips → 24px");
    }
}
