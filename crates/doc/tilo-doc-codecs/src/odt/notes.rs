//! Footnotes/endnotes map ODF's inline `text:note` (`text:note-citation` +
//! `text:note-body`) onto the canonical model: a `note-ref` marker at the note's
//! position in the body paragraph, plus its content as a block sequence in the
//! `notes` container, both directions.

use super::plain_paragraphs;
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

    // Each `text:p`/`text:h` in `text:note-body` becomes a paragraph block.
    plain_paragraphs::import(
        raw,
        Some(b"text:note-body"),
        doc,
        |doc, prev| doc.insert_note_block(&note_id, prev, "paragraph"),
        |doc, block, at, text| doc.splice_note_text(&note_id, block, at, 0, text),
        |_, _, _| None,
    );
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
        plain_paragraphs::render(&note.blocks)
    )
}
