//! What the parts beside `word/document.xml` share: comments, footnotes/endnotes and
//! headers/footers each hold plain `w:p` paragraphs (text, tabs, breaks) inside one
//! element per item.

use crate::xml_escape::push_text;
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use tilo_doc::model::DocMut;
use tilo_doc::snapshot::BlockView;

/// Walk a part, hand each `tag` element's open tag to `open` and — when it returns
/// `Some` — the element's verbatim XML to `close` once it ends.
pub(super) fn scan_items<T>(
    xml: &[u8],
    tag: &[u8],
    mut open: impl FnMut(&BytesStart) -> Option<T>,
    mut close: impl FnMut(T, &str),
) {
    let Ok(s) = std::str::from_utf8(xml) else {
        return;
    };
    let mut reader = Reader::from_str(s);
    // The open item: its key, raw-XML start byte, and open depth.
    let mut capture: Option<(T, usize, i32)> = None;
    loop {
        let pos_before = usize::try_from(reader.buffer_position()).unwrap_or(usize::MAX);
        let Ok(ev) = reader.read_event() else {
            break;
        };
        if let Some((_, start, depth)) = capture.as_mut() {
            match &ev {
                Event::Start(_) => *depth = depth.saturating_add(1),
                Event::End(_) => {
                    *depth = depth.saturating_sub(1);
                    if *depth == 0 {
                        let pos_after =
                            usize::try_from(reader.buffer_position()).unwrap_or(usize::MAX);
                        let raw = s.get(*start..pos_after).unwrap_or_default();
                        if let Some((key, _, _)) = capture.take() {
                            close(key, raw);
                        }
                    }
                }
                Event::Eof => break,
                _ => {}
            }
            continue;
        }
        match ev {
            Event::Start(e) if e.name().as_ref() == tag => {
                capture = open(&e).map(|key| (key, pos_before, 1));
            }
            Event::Eof => break,
            _ => {}
        }
    }
}

/// Import one item's verbatim XML: each `w:p` becomes a block made by `insert_block`
/// (given the previous block), and its `w:t` text, `w:tab` and `w:br`/`w:cr` are appended
/// through `splice` (block, offset, text).
pub(super) fn import_paragraphs(
    raw: &str,
    doc: &mut DocMut,
    insert_block: impl Fn(&mut DocMut, Option<&str>) -> Option<String>,
    splice: impl Fn(&mut DocMut, &str, usize, &str),
) {
    let mut reader = Reader::from_str(raw);
    let mut prev: Option<String> = None;
    let mut cur: Option<String> = None;
    let mut cur_len: usize = 0;
    let mut in_t = false;

    let append = |doc: &mut DocMut, cur: &Option<String>, cur_len: &mut usize, txt: &str| {
        if let Some(b) = cur
            && !txt.is_empty()
        {
            splice(doc, b, *cur_len, txt);
            *cur_len = cur_len.saturating_add(txt.chars().count());
        }
    };

    while let Ok(ev) = reader.read_event() {
        match ev {
            Event::Start(e) => match e.name().as_ref() {
                b"w:p" => {
                    if let Some(id) = insert_block(doc, prev.as_deref()) {
                        prev = Some(id.clone());
                        cur = Some(id);
                        cur_len = 0;
                    }
                }
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
}

/// Render blocks as `w:p` paragraphs of plain runs plus `w:tab`/`w:br` (richer inline
/// styling is not modeled in these parts).
pub(super) fn render_paragraphs(blocks: &[BlockView]) -> String {
    blocks
        .iter()
        .map(|b| render_paragraph(b, |_| None))
        .collect()
}

/// Render one block as a `w:p`; `inline` may replace the character at a char index with
/// raw XML (e.g. a field), which is emitted in place of the character.
pub(super) fn render_paragraph(
    b: &BlockView,
    mut inline: impl FnMut(usize) -> Option<&'static str>,
) -> String {
    let mut out = String::from("<w:p>");
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
    for (i, c) in b.text.chars().enumerate() {
        let element = match c {
            '\t' => "<w:r><w:tab/></w:r>",
            '\n' => "<w:r><w:br/></w:r>",
            _ => {
                let Some(xml) = inline(i) else {
                    buf.push(c);
                    continue;
                };
                xml
            }
        };
        flush(&mut out, &mut buf);
        out.push_str(element);
    }
    flush(&mut out, &mut buf);
    out.push_str("</w:p>");
    out
}

#[cfg(test)]
mod tests {
    use super::scan_items;

    #[test]
    fn a_part_cut_off_inside_an_item_ends_the_scan() {
        let mut closed = Vec::new();
        scan_items(
            br#"<w:comments><w:comment w:id="1"><w:p/></w:comment><w:comment w:id="2"><w:p>"#,
            b"w:comment",
            |_| Some(()),
            |(), raw| closed.push(raw.to_string()),
        );
        assert_eq!(closed, [r#"<w:comment w:id="1"><w:p/></w:comment>"#]);
    }
}
