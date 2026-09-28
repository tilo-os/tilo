//! Multi-column section layout ⇄ OOXML section properties. `WordprocessingML` models
//! newspaper-style columns as `<w:cols w:num="N" w:space="…"/>` inside the body's
//! `<w:sectPr>`; `w:space` is the inter-column gap in twips. This maps that count/gap
//! onto the document-wide column setting; the exporter hands back the `<w:cols>`
//! element to fold into the generated sectPr.

use quick_xml::events::Event;
use quick_xml::reader::Reader;
use tilo_doc::model::{DocMut, DocRef, columns as model_columns};

/// Twips (1/20 pt) per CSS px at 96 dpi: 1 px = 1/96 in = 1440/96 twips = 15 twips.
const TWIPS_PER_PX: f64 = 15.0;

/// Import the body column count + gap from the final `w:sectPr`'s `w:cols` (the body's own
/// section; a section break keeps its columns verbatim with its paragraph). No-op when
/// the section carries no `w:cols` (or a count of one) — the document keeps its
/// single-column default.
pub(super) fn import_section_columns(document_xml: &[u8], doc: &mut DocMut) {
    let Some((count, gap)) = parse_cols(document_xml) else {
        return;
    };
    if count <= 1 {
        return;
    }
    doc.set_columns(count, gap);
}

/// Scan `document.xml` for the `w:cols` of the body's own `w:sectPr` (not a section break's,
/// inside a paragraph's `w:pPr`) and read its `w:num`/`w:space`; `None` when it has none.
fn parse_cols(document_xml: &[u8]) -> Option<(u32, Option<f64>)> {
    let s = std::str::from_utf8(document_xml).ok()?;
    let mut reader = Reader::from_str(s);
    let mut in_sectpr = false;
    let mut in_ppr = 0usize;
    let mut last = None;
    let mut current = None;
    loop {
        let Ok(ev) = reader.read_event() else {
            return last;
        };
        match ev {
            Event::Start(e) if e.name().as_ref() == b"w:pPr" => in_ppr = in_ppr.saturating_add(1),
            Event::End(e) if e.name().as_ref() == b"w:pPr" => in_ppr = in_ppr.saturating_sub(1),
            _ if in_ppr > 0 => {}
            Event::Start(e) if e.name().as_ref() == b"w:sectPr" => {
                in_sectpr = true;
                current = None;
            }
            Event::Empty(e) if e.name().as_ref() == b"w:sectPr" => last = None,
            Event::End(e) if e.name().as_ref() == b"w:sectPr" => {
                in_sectpr = false;
                last = current.take();
            }
            Event::Empty(e) | Event::Start(e) if in_sectpr && e.name().as_ref() == b"w:cols" => {
                let num = e
                    .attributes()
                    .flatten()
                    .find(|a| a.key.as_ref() == b"w:num")
                    .and_then(|a| crate::unescape_attr(&a))
                    .and_then(|v| v.trim().parse::<u32>().ok())
                    .unwrap_or(1);
                let gap = e
                    .attributes()
                    .flatten()
                    .find(|a| a.key.as_ref() == b"w:space")
                    .and_then(|a| crate::unescape_attr(&a))
                    .and_then(|v| v.trim().parse::<f64>().ok())
                    .map(|twips| (twips / TWIPS_PER_PX * 100.0).round() / 100.0);
                current = Some((num, gap));
            }
            Event::Eof => return last,
            _ => {}
        }
    }
}

/// The `<w:cols>` element XML for the document's column setting, or `None` for a single
/// column (needs none). A `w:sectPr`'s defined element order puts `w:cols` after the
/// header/footer references, so the exporter appends this after them.
pub(super) fn section_columns_xml(doc: &DocRef) -> Option<String> {
    let (count, gap) = model_columns(doc.doc, doc.c);
    if count <= 1 {
        return None;
    }
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a column gap in twips, converted from a document px value; far below i64::MAX"
    )]
    let space_attr = gap
        .map(|px| format!(r#" w:space="{}""#, (px * TWIPS_PER_PX).round() as i64))
        .unwrap_or_default();
    Some(format!(r#"<w:cols w:num="{count}"{space_attr}/>"#))
}
