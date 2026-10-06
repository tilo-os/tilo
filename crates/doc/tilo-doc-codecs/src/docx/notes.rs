//! Footnotes / endnotes ⇄ OOXML `word/footnotes.xml` / `word/endnotes.xml`: each kind
//! has its own part of `w:p` paragraphs, referenced from the body via
//! `<w:footnoteReference w:id="N"/>`/`<w:endnoteReference w:id="N"/>`. This maps a note
//! onto a `note-ref` marker in body text plus its content in the `notes` container.

use crate::xml_escape::push_text;
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use std::collections::BTreeMap;
use tilo_doc::model::{DocMut, DocRef, NoteKind, materialize_notes};
use tilo_doc::snapshot::{BlockView, NoteView};
// `write!` into a `String` cannot fail; the discarded `Result` is the
// `fmt::Write` contract, not a swallowed error.
use std::fmt::Write as _;

/// The fixed part names + relationship ids qdoc writes for footnotes/endnotes.
pub(super) const FOOTNOTES_PART: &str = "word/footnotes.xml";
pub(super) const ENDNOTES_PART: &str = "word/endnotes.xml";

/// Import a footnotes/endnotes part, returning a `docx_id → qdoc_note_id` map for the
/// body parser. Reserved separator/continuationSeparator notes hold no user content and
/// are skipped.
pub(super) fn import_notes_part(
    xml: &[u8],
    kind: NoteKind,
    doc: &mut DocMut,
) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let Ok(s) = std::str::from_utf8(xml) else {
        return map;
    };
    let tag: &[u8] = match kind {
        NoteKind::Footnote => b"w:footnote",
        NoteKind::Endnote => b"w:endnote",
    };
    let mut reader = Reader::from_str(s);
    // Per-note capture: the docx id, its raw-XML start byte, and the open depth.
    let mut capture: Option<(String, usize, i32)> = None;
    loop {
        let pos_before = usize::try_from(reader.buffer_position()).unwrap_or(usize::MAX);
        let Ok(ev) = reader.read_event() else {
            break;
        };
        // Advance / close an in-progress note capture.
        if let Some((id, start, depth)) = capture.as_mut() {
            match &ev {
                Event::Start(_) => *depth = depth.saturating_add(1),
                Event::End(_) => {
                    *depth = depth.saturating_sub(1);
                    if *depth == 0 {
                        let pos_after =
                            usize::try_from(reader.buffer_position()).unwrap_or(usize::MAX);
                        let raw = s.get(*start..pos_after).unwrap_or_default().to_string();
                        let note_id = import_one_note(&raw, kind, doc);
                        map.insert(id.clone(), note_id);
                        capture = None;
                    }
                }
                _ => {}
            }
            continue;
        }
        match ev {
            Event::Start(e) if e.name().as_ref() == tag => {
                // A content note has no `w:type` (separators do); capture it verbatim and parse on close.
                let ntype = attr_w_val(&e, b"w:type");
                if ntype.is_none()
                    && let Some(id) = attr_w_val(&e, b"w:id")
                {
                    capture = Some((id, pos_before, 1));
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    map
}

/// Parse one verbatim `<w:footnote>/<w:endnote>` element into a fresh note: each `w:p`
/// becomes a paragraph block, `w:t` runs are text.
fn import_one_note(raw: &str, kind: NoteKind, doc: &mut DocMut) -> String {
    let note_id = doc.insert_note(kind);
    let mut reader = Reader::from_str(raw);
    let mut prev: Option<String> = None;
    let mut cur: Option<String> = None;
    let mut cur_len: usize = 0;
    let mut in_t = false;

    let open_block = |doc: &mut DocMut,
                      prev: &mut Option<String>,
                      cur: &mut Option<String>,
                      cur_len: &mut usize| {
        if let Some(id) = doc.insert_note_block(&note_id, prev.as_deref(), "paragraph") {
            *prev = Some(id.clone());
            *cur = Some(id);
            *cur_len = 0;
        }
    };
    let append = |doc: &mut DocMut, cur: &Option<String>, cur_len: &mut usize, txt: &str| {
        if let Some(b) = cur
            && !txt.is_empty()
        {
            doc.splice_note_text(&note_id, b, *cur_len, 0, txt);
            *cur_len = cur_len.saturating_add(txt.chars().count());
        }
    };

    while let Ok(ev) = reader.read_event() {
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
                if let Ok(txt) = t.xml10_content()
                    && !txt.is_empty()
                {
                    append(doc, &cur, &mut cur_len, &txt);
                }
            }
            Event::GeneralRef(r) if in_t => {
                if let Some(c) = crate::resolve_general_ref(&r) {
                    append(doc, &cur, &mut cur_len, c.encode_utf8(&mut [0; 4]));
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
    note_id
}

fn attr_w_val(e: &quick_xml::events::BytesStart, key: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == key)
        .and_then(|a| crate::unescape_attr(&a))
}

/// Render a note's paragraphs as `w:footnote`/`w:endnote` inner XML (plain text + tabs/
/// breaks; richer inline styling in notes is not modeled).
fn render_note_body(blocks: &[BlockView]) -> String {
    let mut out = String::new();
    for b in blocks {
        out.push_str("<w:p>");
        let mut buf = String::new();
        let flush = |out: &mut String, buf: &mut String| {
            if !buf.is_empty() {
                out.push_str(r#"<w:r><w:t xml:space="preserve">"#);
                for c in buf.chars() {
                    push_text(out, c);
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

/// The exported footnote/endnote parts + the per-kind id maps the body exporter uses;
/// `None` when a kind has no notes.
pub(super) struct ExportedNotes {
    pub footnotes_xml: Option<Vec<u8>>,
    pub endnotes_xml: Option<Vec<u8>>,
    /// qdoc note id → the docx numeric id emitted in the reference + part.
    pub id_map: BTreeMap<String, i64>,
    /// qdoc note id → kind, so the body emits the right reference element.
    pub kind_map: BTreeMap<String, NoteKind>,
}

/// Footnote and endnote ids each start at 1 (independent id spaces, as Word uses);
/// reserved separator notes (id 0/-1) are emitted so Word renders the separator line.
pub(super) fn export_notes(doc: &DocRef) -> ExportedNotes {
    let notes = materialize_notes(doc.doc, doc.c);
    let mut id_map = BTreeMap::new();
    let mut kind_map = BTreeMap::new();

    let footnotes: Vec<ExportNote> =
        collect_kind(&notes, NoteKind::Footnote, &mut id_map, &mut kind_map);
    let endnotes: Vec<ExportNote> =
        collect_kind(&notes, NoteKind::Endnote, &mut id_map, &mut kind_map);

    ExportedNotes {
        footnotes_xml: part_xml(&footnotes, "footnotes", "footnote"),
        endnotes_xml: part_xml(&endnotes, "endnotes", "endnote"),
        id_map,
        kind_map,
    }
}

fn collect_kind<'a>(
    notes: &'a [NoteView],
    kind: NoteKind,
    id_map: &mut BTreeMap<String, i64>,
    kind_map: &mut BTreeMap<String, NoteKind>,
) -> Vec<ExportNote<'a>> {
    let want = kind.as_str();
    let mut out = Vec::new();
    for (i, n) in notes.iter().filter(|n| n.kind == want).enumerate() {
        // Footnote/endnote ids are 1-based.
        let next = i64::try_from(i).unwrap_or(i64::MAX).saturating_add(1);
        id_map.insert(n.id.clone(), next);
        kind_map.insert(n.id.clone(), kind);
        out.push(ExportNote {
            docx_id: next,
            note: n,
        });
    }
    out
}

/// The full footnotes/endnotes part bytes, or `None` when empty; includes the reserved
/// separator notes ahead of the content notes.
fn part_xml(notes: &[ExportNote], root: &str, item: &str) -> Option<Vec<u8>> {
    if notes.is_empty() {
        return None;
    }
    let mut inner = String::new();
    // Reserved separator notes (no user content; Word expects them).
    let _ = write!(
        inner,
        r#"<w:{item} w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:{item}>"#
    );
    let _ = write!(
        inner,
        r#"<w:{item} w:type="continuationSeparator" w:id="0"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:{item}>"#
    );
    for en in notes {
        let _ = write!(
            inner,
            r#"<w:{item} w:id="{}">{}</w:{item}>"#,
            en.docx_id,
            render_note_body(&en.note.blocks)
        );
    }
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:{root} xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">{inner}</w:{root}>"#
    );
    Some(xml.into_bytes())
}
