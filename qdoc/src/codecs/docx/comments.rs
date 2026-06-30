//! Comments / annotations ⇄ OOXML `word/comments.xml`. WordprocessingML stores
//! comment content in a dedicated `word/comments.xml` part as `<w:comment w:id w:author
//! w:date>` elements, each a sequence of `w:p` paragraphs. The body marks the
//! commented range with `<w:commentRangeStart w:id="N"/>` / `<w:commentRangeEnd
//! w:id="N"/>` and references it via a `<w:commentReference w:id="N"/>` run. The part
//! is wired via `[Content_Types].xml` + `word/_rels/document.xml.rels`. This maps a
//! comment onto the canonical model: a `comment` mark over the body range carrying the
//! comment id, plus the comment's author/date/content in the `comments` container.

use crate::model::{materialize_comments, DocMut, DocRef};
use crate::snapshot::BlockView;
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use std::collections::BTreeMap;

/// The fixed part name + relationship id qdoc writes for comments.
pub(super) const COMMENTS_PART: &str = "word/comments.xml";

/// Import `word/comments.xml` into the comments container, returning a `docx_id →
/// qdoc_comment_id` map so the body parser can wire each `commentRangeStart/End` (by
/// docx id) onto the canonical comment.
pub(super) fn import_comments_part(xml: &[u8], doc: &mut DocMut) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let Ok(s) = std::str::from_utf8(xml) else { return map };
    let mut reader = Reader::from_str(s);
    // Per-comment capture: (docx id, author, date, raw-XML start byte, open depth).
    let mut capture: Option<(String, String, String, usize, i32)> = None;
    loop {
        let pos_before = reader.buffer_position() as usize;
        let ev = match reader.read_event() {
            Ok(e) => e,
            Err(_) => break,
        };
        if let Some((id, author, date, start, depth)) = capture.as_mut() {
            match &ev {
                Event::Start(_) => *depth += 1,
                Event::End(_) => {
                    *depth -= 1;
                    if *depth == 0 {
                        let raw = s[*start..reader.buffer_position() as usize].to_string();
                        if let Some(cid) = import_one_comment(&raw, author, date, doc) {
                            map.insert(id.clone(), cid);
                        }
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

/// Parse one verbatim `<w:comment>` element into a fresh comment (author/date taken
/// from the open tag's attributes) and return its qdoc id. Each `w:p` becomes a
/// paragraph block; `w:t` runs are text, `w:tab`/`w:br` map onto tab/newline.
fn import_one_comment(raw: &str, author: &str, date: &str, doc: &mut DocMut) -> Option<String> {
    let comment_id = doc.insert_comment(author, date);
    let mut reader = Reader::from_str(raw);
    let mut prev: Option<String> = None;
    let mut cur: Option<String> = None;
    let mut cur_len: usize = 0;
    let mut in_t = false;

    let open_block = |doc: &mut DocMut, prev: &mut Option<String>, cur: &mut Option<String>, cur_len: &mut usize| {
        if let Some(id) = doc.insert_comment_block(&comment_id, prev.as_deref(), "paragraph") {
            *prev = Some(id.clone());
            *cur = Some(id);
            *cur_len = 0;
        }
    };
    let append = |doc: &mut DocMut, cur: &Option<String>, cur_len: &mut usize, txt: &str| {
        if let Some(b) = cur {
            if !txt.is_empty() {
                doc.splice_comment_text(&comment_id, b, *cur_len, 0, txt);
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
                if let Ok(txt) = t.unescape() {
                    if !txt.is_empty() {
                        append(doc, &cur, &mut cur_len, &txt);
                    }
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
    Some(comment_id)
}

fn attr_w_val(e: &quick_xml::events::BytesStart, key: &[u8]) -> Option<String> {
    e.attributes().flatten().find(|a| a.key.as_ref() == key).and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
}

fn push_escaped(out: &mut String, c: char) {
    match c {
        '&' => out.push_str("&amp;"),
        '<' => out.push_str("&lt;"),
        '>' => out.push_str("&gt;"),
        _ => out.push(c),
    }
}

fn escape_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// Render a comment's content paragraphs as the inner XML of a `w:comment` element
/// (plain text + tabs/breaks; richer inline styling in comments is not modeled).
fn render_comment_body(blocks: &[BlockView]) -> String {
    let mut out = String::new();
    for b in blocks {
        out.push_str("<w:p>");
        let mut buf = String::new();
        let flush = |out: &mut String, buf: &mut String| {
            if !buf.is_empty() {
                out.push_str(r#"<w:r><w:t xml:space="preserve">"#);
                for c in buf.chars() {
                    push_escaped(out, c);
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

/// The exported `word/comments.xml` part + the `qdoc-id → docx-id` map the body
/// exporter uses to emit `commentRangeStart/End` + `commentReference`. The part is
/// `None` when the document has no comments.
pub(super) struct ExportedComments {
    pub comments_xml: Option<Vec<u8>>,
    /// qdoc comment id → the docx numeric id emitted in the body + part.
    pub id_map: BTreeMap<String, i64>,
}

/// Build the comments part from the document's comments, assigning docx ids from 0.
pub(super) fn export_comments(doc: &DocRef) -> ExportedComments {
    let comments = materialize_comments(doc.doc, doc.c);
    let mut id_map = BTreeMap::new();
    if comments.is_empty() {
        return ExportedComments { comments_xml: None, id_map };
    }
    let mut inner = String::new();
    for (i, c) in comments.iter().enumerate() {
        let docx_id = i as i64;
        id_map.insert(c.id.clone(), docx_id);
        let date = if c.date.is_empty() { String::new() } else { format!(r#" w:date="{}""#, escape_attr(&c.date)) };
        inner.push_str(&format!(
            r#"<w:comment w:id="{docx_id}" w:author="{}"{date} w:initials="">{}</w:comment>"#,
            escape_attr(&c.author),
            render_comment_body(&c.blocks),
        ));
    }
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">{inner}</w:comments>"#
    );
    ExportedComments { comments_xml: Some(xml.into_bytes()), id_map }
}

/// The `<w:commentReference>` run for a comment id (its citation marker), or `None`
/// when the comment isn't being exported.
pub(super) fn comment_reference_run(comment_id: &str, id_map: &BTreeMap<String, i64>) -> Option<String> {
    let docx_id = id_map.get(comment_id)?;
    Some(format!(
        r#"<w:r><w:rPr><w:rStyle w:val="CommentReference"/></w:rPr><w:commentReference w:id="{docx_id}"/></w:r>"#
    ))
}

/// The `<w:commentRangeStart>` empty element for a comment id, or `None` when absent.
pub(super) fn comment_range_start(comment_id: &str, id_map: &BTreeMap<String, i64>) -> Option<String> {
    let docx_id = id_map.get(comment_id)?;
    Some(format!(r#"<w:commentRangeStart w:id="{docx_id}"/>"#))
}

/// The `<w:commentRangeEnd>` empty element for a comment id, or `None` when absent.
pub(super) fn comment_range_end(comment_id: &str, id_map: &BTreeMap<String, i64>) -> Option<String> {
    let docx_id = id_map.get(comment_id)?;
    Some(format!(r#"<w:commentRangeEnd w:id="{docx_id}"/>"#))
}
