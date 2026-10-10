//! Footnotes / endnotes ⇄ OOXML `word/footnotes.xml` / `word/endnotes.xml`: each kind
//! has its own part of `w:p` paragraphs, referenced from the body via
//! `<w:footnoteReference w:id="N"/>`/`<w:endnoteReference w:id="N"/>`. This maps a note
//! onto a `note-ref` marker in body text plus its content in the `notes` container.

use super::side_part;
use super::styles::attr_val;
use std::collections::BTreeMap;
use tilo_doc::model::{DocMut, DocRef, NoteKind, materialize_notes};
use tilo_doc::snapshot::NoteView;
// `write!` into a `String` cannot fail; the discarded `Result` is the
// `fmt::Write` contract, not a swallowed error.
use std::fmt::Write as _;

/// The fixed part names + relationship ids qdoc writes for footnotes/endnotes.
pub(super) const FOOTNOTES_PART: &str = "word/footnotes.xml";
pub(super) const ENDNOTES_PART: &str = "word/endnotes.xml";

/// Import a footnotes/endnotes part, returning a `docx_id → qdoc_note_id` map for the
/// body parser; each `w:p` of a note becomes a paragraph block. Reserved
/// separator/continuationSeparator notes (those with a `w:type`) hold no user content and
/// are skipped.
pub(super) fn import_notes_part(
    xml: &[u8],
    kind: NoteKind,
    doc: &mut DocMut,
) -> BTreeMap<String, String> {
    let tag: &[u8] = match kind {
        NoteKind::Footnote => b"w:footnote",
        NoteKind::Endnote => b"w:endnote",
    };
    let mut map = BTreeMap::new();
    side_part::scan_items(
        xml,
        tag,
        |e| {
            if attr_val(e, b"w:type").is_some() {
                return None;
            }
            attr_val(e, b"w:id")
        },
        |id, raw| {
            let note_id = doc.insert_note(kind);
            side_part::import_paragraphs(
                raw,
                doc,
                |doc, prev| doc.insert_note_block(&note_id, prev, "paragraph"),
                |doc, block, at, text| doc.splice_note_text(&note_id, block, at, 0, text),
            );
            map.insert(id, note_id);
        },
    );
    map
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
            side_part::render_paragraphs(&en.note.blocks)
        );
    }
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:{root} xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">{inner}</w:{root}>"#
    );
    Some(xml.into_bytes())
}
