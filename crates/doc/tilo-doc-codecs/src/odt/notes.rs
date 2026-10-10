//! Footnotes/endnotes map ODF's inline `text:note` (`text:note-citation` +
//! `text:note-body`) onto the canonical model: a `note-ref` marker at the note's
//! position in the body paragraph, plus its content as a block sequence in the
//! `notes` container, both directions.

use super::comments::render_plain_body;
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use tilo_doc::model::{DocMut, NoteKind};
use tilo_doc::snapshot::NoteView;

/// Parse one `<text:note>` element into a fresh note, importing its body paragraphs
/// into blocks; returns the new note id. Citation text is recomputed by renderers,
/// not modeled here.
pub(super) fn import_note(raw: &str, doc: &mut DocMut) -> String {
    let kind = note_class(raw).map_or(NoteKind::Footnote, |c| NoteKind::from_str(&c));
    let note_id = doc.insert_note(kind);

    // Each `text:p`/`text:h` in `text:note-body` becomes a paragraph block; text/tabs/breaks/spaces map on.
    let mut reader = Reader::from_str(raw);
    let mut in_body = false;
    let mut prev: Option<String> = None;
    let mut cur: Option<String> = None;
    let mut cur_len: usize = 0;

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
                b"text:note-body" => in_body = true,
                b"text:p" | b"text:h" if in_body => {
                    open_block(doc, &mut prev, &mut cur, &mut cur_len);
                }
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
                        .and_then(|a| crate::unescape_attr(&a))
                        .and_then(|v| v.parse::<usize>().ok())
                        .unwrap_or(1)
                        .min(256);
                    append(doc, &cur, &mut cur_len, &" ".repeat(n));
                }
                _ => {}
            },
            Event::Text(t) if in_body && cur.is_some() => {
                if let Ok(txt) = t.xml10_content() {
                    // Skip pretty-printer indentation between structural elements.
                    if !txt.is_empty() && !super::content::is_layout_whitespace(&txt) {
                        append(doc, &cur, &mut cur_len, &txt);
                    }
                }
            }
            Event::GeneralRef(r) if in_body && cur.is_some() => {
                if let Some(c) = crate::resolve_general_ref(&r) {
                    append(doc, &cur, &mut cur_len, c.encode_utf8(&mut [0; 4]));
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
    note_id
}

/// Read the `text:note-class` attribute from a raw `<text:note …>` open tag.
fn note_class(raw: &str) -> Option<String> {
    let mut reader = Reader::from_str(raw);
    loop {
        match reader.read_event() {
            Ok(Event::Start(e) | Event::Empty(e)) if e.name().as_ref() == b"text:note" => {
                return e
                    .attributes()
                    .flatten()
                    .find(|a| a.key.as_ref() == b"text:note-class")
                    .and_then(|a| crate::unescape_attr(&a));
            }
            Ok(Event::Eof) | Err(_) => return None,
            _ => {}
        }
    }
}

/// Full inline `<text:note>` XML for a note; `n` is the 1-based citation number, recomputed on export from reference order.
pub(super) fn render_inline_note(note: &NoteView, n: usize) -> String {
    let kind = NoteKind::from_str(&note.kind);
    let class = kind.as_str();
    let id_prefix = if matches!(kind, NoteKind::Endnote) {
        "endnote"
    } else {
        "ftn"
    };
    format!(
        r#"<text:note text:id="{id_prefix}{n}" text:note-class="{class}"><text:note-citation>{n}</text:note-citation><text:note-body>{}</text:note-body></text:note>"#,
        render_plain_body(&note.blocks)
    )
}
