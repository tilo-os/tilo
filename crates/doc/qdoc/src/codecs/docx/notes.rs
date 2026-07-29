//! Footnotes / endnotes ⇄ OOXML `word/footnotes.xml` / `word/endnotes.xml`.
//! WordprocessingML stores each note kind in its own part: `w:footnotes` holds
//! `<w:footnote w:id="N">` elements, `w:endnotes` holds `<w:endnote w:id="N">`,
//! each a sequence of `w:p` paragraphs. The body references a note from inside a
//! run via `<w:footnoteReference w:id="N"/>` / `<w:endnoteReference w:id="N"/>`.
//! The parts are wired via `[Content_Types].xml` + `word/_rels/document.xml.rels`.
//! This maps a note onto the canonical model: a `note-ref` marker (placeholder +
//! `note-ref` mark valued with the note id) at the reference position in body text,
//! plus the note's content as a block sequence in the `notes` container.

use crate::model::{materialize_notes, DocMut, DocRef, NoteKind};
use crate::snapshot::{BlockView, NoteView};
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use std::collections::BTreeMap;

/// The fixed part names + relationship ids qdoc writes for footnotes/endnotes.
pub(super) const FOOTNOTES_PART: &str = "word/footnotes.xml";
pub(super) const ENDNOTES_PART: &str = "word/endnotes.xml";
pub(super) const FOOTNOTES_REL_ID: &str = "rIdQdocFtnotes";
pub(super) const ENDNOTES_REL_ID: &str = "rIdQdocEndnotes";

/// Import a `word/footnotes.xml` / `word/endnotes.xml` part into the notes
/// container, returning a `docx_id → qdoc_note_id` map so the body parser can wire
/// references. `kind` selects the element tag (`w:footnote`/`w:endnote`). The
/// reserved separator / continuationSeparator notes (carrying a `w:type`) are
/// skipped — they hold no user content.
pub(super) fn import_notes_part(xml: &[u8], kind: NoteKind, doc: &mut DocMut) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let Ok(s) = std::str::from_utf8(xml) else { return map };
    let tag: &[u8] = match kind {
        NoteKind::Footnote => b"w:footnote",
        NoteKind::Endnote => b"w:endnote",
    };
    let mut reader = Reader::from_str(s);
    // Per-note capture: the docx id, its raw-XML start byte, and the open depth.
    let mut capture: Option<(String, usize, i32)> = None;
    loop {
        let pos_before = reader.buffer_position() as usize;
        let ev = match reader.read_event() {
            Ok(e) => e,
            Err(_) => break,
        };
        // Advance / close an in-progress note capture.
        if let Some((id, start, depth)) = capture.as_mut() {
            match &ev {
                Event::Start(_) => *depth += 1,
                Event::End(_) => {
                    *depth -= 1;
                    if *depth == 0 {
                        let raw = s[*start..reader.buffer_position() as usize].to_string();
                        if let Some(note_id) = import_one_note(&raw, kind, doc) {
                            map.insert(id.clone(), note_id);
                        }
                        capture = None;
                    }
                }
                _ => {}
            }
            continue;
        }
        match ev {
            Event::Start(e) if e.name().as_ref() == tag => {
                // A content note has no `w:type` (separators do). Capture the
                // element verbatim; parse its paragraphs on close.
                let ntype = attr_w_val(&e, b"w:type");
                if ntype.is_none() {
                    if let Some(id) = attr_w_val(&e, b"w:id") {
                        capture = Some((id, pos_before, 1));
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    map
}

/// Parse one verbatim `<w:footnote>/<w:endnote>` element into a fresh note and
/// return its qdoc id. Each `w:p` becomes a paragraph block; `w:t` runs are text,
/// `w:tab`/`w:br` map onto tab/newline.
fn import_one_note(raw: &str, kind: NoteKind, doc: &mut DocMut) -> Option<String> {
    let note_id = doc.insert_note(kind);
    let mut reader = Reader::from_str(raw);
    let mut prev: Option<String> = None;
    let mut cur: Option<String> = None;
    let mut cur_len: usize = 0;
    let mut in_t = false;

    let open_block = |doc: &mut DocMut, prev: &mut Option<String>, cur: &mut Option<String>, cur_len: &mut usize| {
        if let Some(id) = doc.insert_note_block(&note_id, prev.as_deref(), "paragraph") {
            *prev = Some(id.clone());
            *cur = Some(id);
            *cur_len = 0;
        }
    };
    let append = |doc: &mut DocMut, cur: &Option<String>, cur_len: &mut usize, txt: &str| {
        if let Some(b) = cur {
            if !txt.is_empty() {
                doc.splice_note_text(&note_id, b, *cur_len, 0, txt);
                *cur_len += txt.chars().count();
            }
        }
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
                _ => {}
            },
            Event::Empty(e) => match e.name().as_ref() {
                b"w:tab" => append(doc, &cur, &mut cur_len, "\t"),
                b"w:br" | b"w:cr" => append(doc, &cur, &mut cur_len, "\n"),
                _ => {}
            },
            Event::Text(t) if in_t => {
                if let Ok(txt) = t.xml10_content() {
                    if !txt.is_empty() {
                        append(doc, &cur, &mut cur_len, &txt);
                    }
                }
            }
            Event::End(e) => match e.name().as_ref() {
                b"w:t" => in_t = false,
                b"w:p" => cur = None,
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    Some(note_id)
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

/// Render a note's content paragraphs as the inner XML of a `w:footnote`/`w:endnote`
/// element (plain text + tabs/breaks; richer inline styling in notes is not
/// modeled).
fn render_note_body(blocks: &[BlockView]) -> String {
    let mut out = String::new();
    for b in blocks {
        out.push_str("<w:p>");
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
        for c in b.text.chars() {
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
    }
    out
}

/// A note assigned a stable `w:id` for the body references, paired with its view.
pub(super) struct ExportNote<'a> {
    pub docx_id: i64,
    pub note: &'a NoteView,
}

/// The exported footnote/endnote parts + the per-kind `qdoc-id → docx-id` maps the
/// body exporter uses to emit references. Each part is `None` when its kind has no
/// notes.
pub(super) struct ExportedNotes {
    pub footnotes_xml: Option<Vec<u8>>,
    pub endnotes_xml: Option<Vec<u8>>,
    /// qdoc note id → the docx numeric id emitted in the reference + part.
    pub id_map: BTreeMap<String, i64>,
    /// qdoc note id → kind, so the body emits the right reference element.
    pub kind_map: BTreeMap<String, NoteKind>,
}

impl ExportedNotes {
    pub fn any(&self) -> bool {
        self.footnotes_xml.is_some() || self.endnotes_xml.is_some()
    }
}

/// Build the footnote/endnote parts from the document's notes. Footnote ids start
/// at 1, endnote ids start at 1 (independent id spaces, as Word uses). The reserved
/// separator notes (id 0 / -1) are emitted so Word renders the separator line.
pub(super) fn export_notes(doc: &DocRef) -> ExportedNotes {
    let notes = materialize_notes(doc.doc, doc.c);
    let mut id_map = BTreeMap::new();
    let mut kind_map = BTreeMap::new();

    let footnotes: Vec<ExportNote> = collect_kind(&notes, NoteKind::Footnote, &mut id_map, &mut kind_map);
    let endnotes: Vec<ExportNote> = collect_kind(&notes, NoteKind::Endnote, &mut id_map, &mut kind_map);

    ExportedNotes {
        footnotes_xml: part_xml(&footnotes, "footnotes", "footnote"),
        endnotes_xml: part_xml(&endnotes, "endnotes", "endnote"),
        id_map,
        kind_map,
    }
}

/// Assign docx ids (from 1) to the notes of one kind and record the id/kind maps.
fn collect_kind<'a>(
    notes: &'a [NoteView],
    kind: NoteKind,
    id_map: &mut BTreeMap<String, i64>,
    kind_map: &mut BTreeMap<String, NoteKind>,
) -> Vec<ExportNote<'a>> {
    let want = kind.as_str();
    let mut out = Vec::new();
    let mut next = 1i64;
    for n in notes.iter().filter(|n| n.kind == want) {
        id_map.insert(n.id.clone(), next);
        kind_map.insert(n.id.clone(), kind);
        out.push(ExportNote { docx_id: next, note: n });
        next += 1;
    }
    out
}

/// The full `word/footnotes.xml`/`word/endnotes.xml` bytes, or `None` when empty.
/// Includes the reserved separator + continuationSeparator notes (ids -1 and 0) so
/// Word renders the divider line, then the content notes.
fn part_xml(notes: &[ExportNote], root: &str, item: &str) -> Option<Vec<u8>> {
    if notes.is_empty() {
        return None;
    }
    let mut inner = String::new();
    // Reserved separator notes (no user content; Word expects them).
    inner.push_str(&format!(
        r#"<w:{item} w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:{item}>"#
    ));
    inner.push_str(&format!(
        r#"<w:{item} w:type="continuationSeparator" w:id="0"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:{item}>"#
    ));
    for en in notes {
        inner.push_str(&format!(r#"<w:{item} w:id="{}">{}</w:{item}>"#, en.docx_id, render_note_body(&en.note.blocks)));
    }
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:{root} xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">{inner}</w:{root}>"#
    );
    Some(xml.into_bytes())
}
