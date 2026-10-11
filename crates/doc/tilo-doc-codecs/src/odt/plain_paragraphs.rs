//! The plain `text:p`/`text:h` paragraphs (text, tabs, breaks, spaces) that comments,
//! notes and page headers/footers hold; richer inline styling is not modeled there.

use super::styles::attr_val;
use crate::xml_escape::push_text;
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use tilo_doc::model::DocMut;
use tilo_doc::snapshot::BlockView;

/// Import the paragraphs of `xml` — only those inside `within` elements when given. Each
/// becomes a block made by `insert_block` (given the previous block); its text is appended
/// through `splice` (block, offset, text). Any other empty element inside a paragraph is
/// offered to `inline` (name, block, offset), which may return text standing for it.
pub(super) fn import(
    xml: &str,
    within: Option<&[u8]>,
    doc: &mut DocMut,
    insert_block: impl Fn(&mut DocMut, Option<&str>) -> Option<String>,
    splice: impl Fn(&mut DocMut, &str, usize, &str),
    mut inline: impl FnMut(&[u8], &str, usize) -> Option<String>,
) {
    let mut reader = Reader::from_str(xml);
    let mut inside = within.is_none();
    let mut prev: Option<String> = None;
    let mut cur: Option<String> = None;
    let mut cur_len: usize = 0;

    let append = |doc: &mut DocMut, cur: &Option<String>, cur_len: &mut usize, txt: &str| {
        if let Some(b) = cur
            && !txt.is_empty()
        {
            splice(doc, b, *cur_len, txt);
            *cur_len = cur_len.saturating_add(txt.chars().count());
        }
    };
    let open_block = |doc: &mut DocMut, prev: &mut Option<String>| {
        let id = insert_block(doc, prev.as_deref());
        prev.clone_from(&id);
        id
    };

    while let Ok(ev) = reader.read_event() {
        match ev {
            Event::Start(e) => match e.name().as_ref() {
                n if Some(n) == within => inside = true,
                b"text:p" | b"text:h" if inside => {
                    cur = open_block(doc, &mut prev);
                    cur_len = 0;
                }
                _ => {}
            },
            Event::Empty(e) if inside => match e.name().as_ref() {
                b"text:p" | b"text:h" => {
                    open_block(doc, &mut prev);
                    cur = None;
                }
                b"text:tab" => append(doc, &cur, &mut cur_len, "\t"),
                b"text:line-break" => append(doc, &cur, &mut cur_len, "\n"),
                b"text:s" => {
                    let n = attr_val(&e, b"text:c")
                        .and_then(|v| v.parse::<usize>().ok())
                        .unwrap_or(1)
                        .min(256);
                    append(doc, &cur, &mut cur_len, &" ".repeat(n));
                }
                name => {
                    if let Some(b) = cur.as_deref()
                        && let Some(txt) = inline(name, b, cur_len)
                    {
                        append(doc, &cur, &mut cur_len, &txt);
                    }
                }
            },
            Event::Text(t) if inside && cur.is_some() => {
                if let Ok(txt) = t.xml10_content() {
                    // Skip pretty-printer indentation between structural elements.
                    if !super::content::is_layout_whitespace(&txt) {
                        append(doc, &cur, &mut cur_len, &txt);
                    }
                }
            }
            Event::GeneralRef(r) if inside && cur.is_some() => {
                if let Some(c) = crate::resolve_general_ref(&r) {
                    append(doc, &cur, &mut cur_len, c.encode_utf8(&mut [0; 4]));
                }
            }
            Event::End(e) => match e.name().as_ref() {
                n if Some(n) == within => inside = false,
                b"text:p" | b"text:h" if inside => cur = None,
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
}

/// Render blocks as plain `text:p` paragraphs.
pub(super) fn render(blocks: &[BlockView]) -> String {
    blocks.iter().map(|b| render_one(b, |_| None)).collect()
}

/// Render one block as a `text:p`; `inline` may replace the character at a char index
/// with raw XML (e.g. a field), which is emitted in place of the character.
pub(super) fn render_one(
    b: &BlockView,
    mut inline: impl FnMut(usize) -> Option<&'static str>,
) -> String {
    let mut out = String::from("<text:p>");
    for (i, c) in b.text.chars().enumerate() {
        match c {
            '\t' => out.push_str("<text:tab/>"),
            '\n' => out.push_str("<text:line-break/>"),
            _ => match inline(i) {
                Some(xml) => out.push_str(xml),
                None => push_text(&mut out, c),
            },
        }
    }
    out.push_str("</text:p>");
    out
}
