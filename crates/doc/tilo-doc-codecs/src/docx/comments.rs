//! Comments / annotations ⇄ OOXML `word/comments.xml`: `<w:comment w:id w:author w:date>`
//! elements of `w:p` paragraphs, referenced from the body by `<w:commentRangeStart/End
//! w:id="N"/>` bracketing the range and a `<w:commentReference w:id="N"/>` run. This maps
//! a comment onto a `comment` mark over the body range carrying the comment id, plus its
//! author/date/content in the `comments` container.

use crate::xml_escape::{escape_attr, push_text};
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use std::collections::BTreeMap;
use tilo_doc::model::{DocMut, DocRef, materialize_comments};
use tilo_doc::snapshot::BlockView;
// `write!` into a `String` cannot fail; the discarded `Result` is the
// `fmt::Write` contract, not a swallowed error.
use std::fmt::Write as _;

/// The fixed part name + relationship id qdoc writes for comments.
pub(super) const COMMENTS_PART: &str = "word/comments.xml";

/// Import `word/comments.xml`, returning a `docx_id → qdoc_comment_id` map so the body
/// parser can wire each `commentRangeStart/End` onto the canonical comment.
pub(super) fn import_comments_part(xml: &[u8], doc: &mut DocMut) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let Ok(s) = std::str::from_utf8(xml) else {
        return map;
    };
    let mut reader = Reader::from_str(s);
    // Per-comment capture: (docx id, author, date, raw-XML start byte, open depth).
    let mut capture: Option<(String, String, String, usize, i32)> = None;
    loop {
        let pos_before = usize::try_from(reader.buffer_position()).unwrap_or(usize::MAX);
        let Ok(ev) = reader.read_event() else {
            break;
        };
        if let Some((id, author, date, start, depth)) = capture.as_mut() {
            match &ev {
                Event::Start(_) => *depth = depth.saturating_add(1),
                Event::End(_) => {
                    *depth = depth.saturating_sub(1);
                    if *depth == 0 {
                        let pos_after =
                            usize::try_from(reader.buffer_position()).unwrap_or(usize::MAX);
                        let raw = s.get(*start..pos_after).unwrap_or_default().to_string();
                        let cid = import_one_comment(&raw, author, date, doc);
                        map.insert(id.clone(), cid);
                        capture = None;
                    }
                }
                _ => {}
            }
            continue;
        }
        match ev {
            Event::Start(e) if e.name().as_ref() == b"w:comment" => {
                if let Some(id) = attr_w_val(&e, b"w:id") {
                    let author = attr_w_val(&e, b"w:author").unwrap_or_default();
                    let date = attr_w_val(&e, b"w:date").unwrap_or_default();
                    capture = Some((id, author, date, pos_before, 1));
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    map
}

/// Parse one verbatim `<w:comment>` element into a fresh comment (author/date from the
/// open tag) and return its qdoc id: each `w:p` becomes a paragraph block.
fn import_one_comment(raw: &str, author: &str, date: &str, doc: &mut DocMut) -> String {
    let comment_id = doc.insert_comment(author, date);
    let mut reader = Reader::from_str(raw);
    let mut prev: Option<String> = None;
    let mut cur: Option<String> = None;
    let mut cur_len: usize = 0;
    let mut in_t = false;

    let open_block = |doc: &mut DocMut,
                      prev: &mut Option<String>,
                      cur: &mut Option<String>,
                      cur_len: &mut usize| {
        if let Some(id) = doc.insert_comment_block(&comment_id, prev.as_deref(), "paragraph") {
            *prev = Some(id.clone());
            *cur = Some(id);
            *cur_len = 0;
        }
    };
    let append = |doc: &mut DocMut, cur: &Option<String>, cur_len: &mut usize, txt: &str| {
        if let Some(b) = cur
            && !txt.is_empty()
        {
            doc.splice_comment_text(&comment_id, b, *cur_len, 0, txt);
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
    comment_id
}

pub(super) fn attr_w_val(e: &quick_xml::events::BytesStart, key: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == key)
        .and_then(|a| crate::unescape_attr(&a))
}

/// Render a comment's or note's paragraphs as `w:comment`/`w:footnote`/`w:endnote` inner XML
/// (plain text + tabs/breaks; richer inline styling in comments and notes is not modeled).
pub(super) fn render_plain_body(blocks: &[BlockView]) -> String {
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

/// The exported comments part + the `qdoc-id → docx-id` map the body exporter uses for
/// `commentRangeStart/End` + `commentReference`; `None` when there are no comments.
pub(super) struct ExportedComments {
    pub comments_xml: Option<Vec<u8>>,
    /// qdoc comment id → the docx numeric id emitted in the body + part.
    pub id_map: BTreeMap<String, i64>,
}

pub(super) fn export_comments(doc: &DocRef) -> ExportedComments {
    let comments = materialize_comments(doc.doc, doc.c);
    let mut id_map = BTreeMap::new();
    if comments.is_empty() {
        return ExportedComments {
            comments_xml: None,
            id_map,
        };
    }
    let mut inner = String::new();
    for (i, c) in comments.iter().enumerate() {
        let docx_id = i64::try_from(i).unwrap_or(i64::MAX);
        id_map.insert(c.id.clone(), docx_id);
        let date = if c.date.is_empty() {
            String::new()
        } else {
            format!(r#" w:date="{}""#, escape_attr(&c.date))
        };
        let _ = write!(
            inner,
            r#"<w:comment w:id="{docx_id}" w:author="{}"{date} w:initials="">{}</w:comment>"#,
            escape_attr(&c.author),
            render_plain_body(&c.blocks),
        );
    }
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">{inner}</w:comments>"#
    );
    ExportedComments {
        comments_xml: Some(xml.into_bytes()),
        id_map,
    }
}

/// The `<w:commentReference>` run for a comment id, or `None` when it isn't being exported.
pub(super) fn comment_reference_run(
    comment_id: &str,
    id_map: &BTreeMap<String, i64>,
) -> Option<String> {
    let docx_id = id_map.get(comment_id)?;
    Some(format!(
        r#"<w:r><w:rPr><w:rStyle w:val="CommentReference"/></w:rPr><w:commentReference w:id="{docx_id}"/></w:r>"#
    ))
}

/// The `<w:commentRangeStart>` empty element for a comment id, or `None` when absent.
pub(super) fn comment_range_start(
    comment_id: &str,
    id_map: &BTreeMap<String, i64>,
) -> Option<String> {
    let docx_id = id_map.get(comment_id)?;
    Some(format!(r#"<w:commentRangeStart w:id="{docx_id}"/>"#))
}

/// The `<w:commentRangeEnd>` empty element for a comment id, or `None` when absent.
pub(super) fn comment_range_end(
    comment_id: &str,
    id_map: &BTreeMap<String, i64>,
) -> Option<String> {
    let docx_id = id_map.get(comment_id)?;
    Some(format!(r#"<w:commentRangeEnd w:id="{docx_id}"/>"#))
}
