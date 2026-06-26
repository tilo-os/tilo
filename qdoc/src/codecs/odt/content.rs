//! Parse the `content.xml` `office:text` body into canonical blocks. Phase 4
//! Task 2 maps the block structure (headings / paragraphs / list items) + text;
//! `text:span` → marks is Task 3, the preservation layer is Task 5.

use crate::format::CodecError;
use crate::model::DocMut;
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;

/// Open a fresh top-level block anchored after the previous one.
fn start_block(
    doc: &mut DocMut,
    prev: &mut Option<String>,
    cur: &mut Option<String>,
    cur_len: &mut usize,
    kind: &str,
) -> String {
    let id = doc.insert_block(prev.as_deref(), kind);
    *prev = Some(id.clone());
    *cur = Some(id.clone());
    *cur_len = 0;
    id
}

fn outline_level(e: &BytesStart) -> Option<i64> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == b"text:outline-level")
        .and_then(|a| a.unescape_value().ok().and_then(|v| v.parse::<i64>().ok()))
}

pub(crate) fn parse_body(content_xml: &[u8], doc: &mut DocMut) -> Result<(), CodecError> {
    let s = std::str::from_utf8(content_xml).map_err(|e| CodecError::Parse(e.to_string()))?;
    let mut reader = Reader::from_str(s);
    let mut in_text = false;
    let mut prev: Option<String> = None;
    let mut cur: Option<String> = None;
    let mut cur_len: usize = 0;
    let mut in_item = false;

    loop {
        match reader.read_event().map_err(|e| CodecError::Parse(format!("xml: {e}")))? {
            Event::Start(e) => match e.name().as_ref() {
                b"office:text" => in_text = true,
                b"text:h" if in_text => {
                    let id = start_block(doc, &mut prev, &mut cur, &mut cur_len, "heading");
                    doc.set_attr(&id, "level", outline_level(&e).unwrap_or(1));
                }
                b"text:p" if in_text && !in_item => {
                    start_block(doc, &mut prev, &mut cur, &mut cur_len, "paragraph");
                }
                b"text:list-item" if in_text => {
                    let id = start_block(doc, &mut prev, &mut cur, &mut cur_len, "list_item");
                    doc.set_attr(&id, "ordered", false);
                    in_item = true;
                }
                _ => {}
            },
            Event::End(e) => match e.name().as_ref() {
                b"office:text" => in_text = false,
                b"text:h" => cur = None,
                b"text:p" if !in_item => cur = None,
                b"text:list-item" => {
                    cur = None;
                    in_item = false;
                }
                _ => {}
            },
            Event::Empty(e) => match e.name().as_ref() {
                b"text:p" if in_text && !in_item => {
                    start_block(doc, &mut prev, &mut cur, &mut cur_len, "paragraph");
                    cur = None;
                }
                b"text:h" if in_text => {
                    let id = start_block(doc, &mut prev, &mut cur, &mut cur_len, "heading");
                    doc.set_attr(&id, "level", outline_level(&e).unwrap_or(1));
                    cur = None;
                }
                _ => {}
            },
            Event::Text(t) => {
                if let Some(b) = cur.as_ref() {
                    let txt = t.unescape().map_err(|e| CodecError::Parse(format!("text: {e}")))?;
                    if !txt.is_empty() {
                        doc.splice_text(b, cur_len, 0, &txt);
                        cur_len += txt.chars().count();
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::container::minimal_odt;
    use super::super::OdtCodec;
    use crate::{AttrVal, Document, FormatProvider};

    #[test]
    fn imports_headings_paragraphs_lists() {
        let body = concat!(
            r#"<text:h text:outline-level="2">Title</text:h>"#,
            r#"<text:p>Hello</text:p>"#,
            r#"<text:list><text:list-item><text:p>one</text:p></text:list-item>"#,
            r#"<text:list-item><text:p>two</text:p></text:list-item></text:list>"#,
        );
        let odt = minimal_odt(body);
        let mut d = Document::new();
        OdtCodec.import(&odt, &mut d.as_mut()).unwrap();
        let snap = d.materialize();

        assert_eq!(snap.blocks[0].kind, "heading");
        assert_eq!(snap.blocks[0].text, "Title");
        assert_eq!(snap.blocks[0].attrs.get("level"), Some(&AttrVal::Int(2)));
        assert_eq!(snap.blocks[1].kind, "paragraph");
        assert_eq!(snap.blocks[1].text, "Hello");
        assert_eq!(snap.blocks.iter().filter(|b| b.kind == "list_item").count(), 2);
        assert_eq!(snap.blocks[2].text, "one");
        assert_eq!(snap.blocks[3].text, "two");
    }
}
