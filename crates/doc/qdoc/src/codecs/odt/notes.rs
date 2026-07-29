//! Footnotes / endnotes ⇄ ODF inline `text:note`. ODF keeps a note *inline* in the
//! body text as `<text:note text:note-class="footnote"|"endnote">` carrying a
//! `<text:note-citation>` (the displayed number) and a `<text:note-body>` holding
//! the note's paragraphs. This maps a note onto the canonical model: a `note-ref`
//! marker (placeholder char + `note-ref` mark valued with the note id) at the
//! note's position in the body paragraph, plus the note's content as a small block
//! sequence in the `notes` container. Both directions.

use crate::model::{DocMut, NoteKind};
use crate::snapshot::{BlockView, NoteView};
use quick_xml::events::Event;
use quick_xml::reader::Reader;

/// Parse one verbatim `<text:note>…</text:note>` element into a fresh note: create
/// the note (with its class → kind), parse each `text:note-body` paragraph into the
/// note's blocks, and return the new note id (so the caller can place the inline
/// reference marker). The citation text is recomputed by renderers, so it is not
/// modeled.
pub(super) fn import_note(raw: &str, doc: &mut DocMut) -> Option<String> {
    let kind = note_class(raw).map(|c| NoteKind::from_str(&c)).unwrap_or(NoteKind::Footnote);
    let note_id = doc.insert_note(kind);

    // Parse the `text:note-body` paragraphs into the note's content blocks. Each
    // `text:p`/`text:h` becomes a paragraph block; text + tabs/breaks/spaces map on.
    let mut reader = Reader::from_str(raw);
    let mut in_body = false;
    let mut prev: Option<String> = None;
    let mut cur: Option<String> = None;
    let mut cur_len: usize = 0;

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
                b"text:note-body" => in_body = true,
                b"text:p" | b"text:h" if in_body => open_block(doc, &mut prev, &mut cur, &mut cur_len),
                _ => {}
            },
            Event::Empty(e) if in_body => match e.name().as_ref() {
                b"text:p" | b"text:h" => {
                    open_block(doc, &mut prev, &mut cur, &mut cur_len);
                    cur = None;
                }
                b"text:tab" => append(doc, &cur, &mut cur_len, "\t"),
                b"text:line-break" => append(doc, &cur, &mut cur_len, "\n"),
                b"text:s" => {
                    let n = e
                        .attributes()
                        .flatten()
                        .find(|a| a.key.as_ref() == b"text:c")
                        .and_then(|a| a.unescape_value().ok().and_then(|v| v.parse::<usize>().ok()))
                        .unwrap_or(1)
                        .min(256);
                    append(doc, &cur, &mut cur_len, &" ".repeat(n));
                }
                _ => {}
            },
            Event::Text(t) if in_body && cur.is_some() => {
                if let Ok(txt) = t.xml10_content() {
                    // Skip pretty-printer indentation between structural elements.
                    if !txt.is_empty() && !(txt.chars().all(|c| c.is_whitespace()) && txt.contains(['\n', '\r'])) {
                        append(doc, &cur, &mut cur_len, &txt);
                    }
                }
            }
            Event::End(e) => match e.name().as_ref() {
                b"text:note-body" => in_body = false,
                b"text:p" | b"text:h" if in_body => cur = None,
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    Some(note_id)
}

/// Read the `text:note-class` attribute from a raw `<text:note …>` open tag.
fn note_class(raw: &str) -> Option<String> {
    let mut reader = Reader::from_str(raw);
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) if e.name().as_ref() == b"text:note" => {
                return e
                    .attributes()
                    .flatten()
                    .find(|a| a.key.as_ref() == b"text:note-class")
                    .and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()));
            }
            Ok(Event::Eof) | Err(_) => return None,
            _ => {}
        }
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

/// Render a note's content paragraphs as the `text:note-body` inner XML. Only plain
/// text + tabs/breaks are modeled in note bodies (richer inline styling is rare in
/// notes and not modeled here).
fn render_note_body(blocks: &[BlockView]) -> String {
    let mut out = String::new();
    for b in blocks {
        out.push_str("<text:p>");
        for c in b.text.chars() {
            match c {
                '\t' => out.push_str("<text:tab/>"),
                '\n' => out.push_str("<text:line-break/>"),
                _ => push_escaped(&mut out, c),
            }
        }
        out.push_str("</text:p>");
    }
    out
}

/// The full inline `<text:note>` XML for a note referenced from body text. `n` is
/// the 1-based citation number (recomputed on export from the reference order).
pub(super) fn render_inline_note(note: &NoteView, n: usize) -> String {
    let kind = NoteKind::from_str(&note.kind);
    let class = kind.as_str();
    let id_prefix = if matches!(kind, NoteKind::Endnote) { "endnote" } else { "ftn" };
    format!(
        r#"<text:note text:id="{id_prefix}{n}" text:note-class="{class}"><text:note-citation>{n}</text:note-citation><text:note-body>{}</text:note-body></text:note>"#,
        render_note_body(&note.blocks)
    )
}
