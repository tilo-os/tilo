//! Comments ⇄ ODF `office:annotation`. ODF places a comment inline at the start of its
//! range as `<office:annotation office:name="…">` (author/date/body); a ranged comment
//! also emits `<office:annotation-end office:name="…"/>` at the range end, tied to the
//! start via the shared `office:name`. This maps onto a `comment` mark over the body range.

use crate::xml_escape::{escape_attr, escape_text, push_text};
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use tilo_doc::model::DocMut;
use tilo_doc::snapshot::{BlockView, CommentView};

/// Parse one `<office:annotation>` element into a fresh comment, importing its author/date
/// and body paragraphs. Returns `(office:name, comment_id)`; `None` name for a point comment.
pub(super) fn import_annotation(raw: &str, doc: &mut DocMut) -> (Option<String>, String) {
    let name = annotation_name(raw);
    let (author, date) = annotation_meta(raw);
    let comment_id = doc.insert_comment(&author, &date);

    let mut reader = Reader::from_str(raw);
    let mut prev: Option<String> = None;
    let mut cur: Option<String> = None;
    let mut cur_len: usize = 0;
    // Depth of `dc:*`/`meta:*` metadata children whose text must not become body content.
    let mut in_meta = 0usize;

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
                b"dc:creator" | b"dc:date" | b"meta:date-string" => {
                    in_meta = in_meta.saturating_add(1);
                }
                b"text:p" | b"text:h" if in_meta == 0 => {
                    open_block(doc, &mut prev, &mut cur, &mut cur_len);
                }
                _ => {}
            },
            Event::Empty(e) if in_meta == 0 && cur.is_some() => match e.name().as_ref() {
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
            Event::Text(t) if in_meta == 0 && cur.is_some() => {
                if let Ok(txt) = t.xml10_content() {
                    // Skip pretty-printer indentation between structural elements.
                    if !txt.is_empty() && !super::content::is_layout_whitespace(&txt) {
                        append(doc, &cur, &mut cur_len, &txt);
                    }
                }
            }
            Event::GeneralRef(r) if in_meta == 0 && cur.is_some() => {
                if let Some(c) = crate::resolve_general_ref(&r) {
                    append(doc, &cur, &mut cur_len, c.encode_utf8(&mut [0; 4]));
                }
            }
            Event::End(e) => match e.name().as_ref() {
                b"dc:creator" | b"dc:date" | b"meta:date-string" => {
                    in_meta = in_meta.saturating_sub(1);
                }
                b"text:p" | b"text:h" if in_meta == 0 => cur = None,
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    (name, comment_id)
}

/// Read `office:name` from a raw `<office:annotation …>` or `<office:annotation-end …/>` tag.
pub(super) fn annotation_name(raw: &str) -> Option<String> {
    let mut reader = Reader::from_str(raw);
    loop {
        match reader.read_event() {
            Ok(Event::Start(e) | Event::Empty(e))
                if matches!(
                    e.name().as_ref(),
                    b"office:annotation" | b"office:annotation-end"
                ) =>
            {
                return e
                    .attributes()
                    .flatten()
                    .find(|a| a.key.as_ref() == b"office:name")
                    .and_then(|a| crate::unescape_attr(&a));
            }
            Ok(Event::Eof) | Err(_) => return None,
            _ => {}
        }
    }
}

/// Read the author (`dc:creator`) + date (`dc:date`) from an annotation's metadata.
fn annotation_meta(raw: &str) -> (String, String) {
    let mut reader = Reader::from_str(raw);
    let mut author = String::new();
    let mut date = String::new();
    let mut collecting: Option<bool> = None; // Some(true)=creator, Some(false)=date
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => match e.name().as_ref() {
                b"dc:creator" => collecting = Some(true),
                b"dc:date" => collecting = Some(false),
                _ => {}
            },
            Ok(Event::Text(t)) => {
                if let (Some(is_author), Ok(txt)) = (collecting, t.xml10_content()) {
                    if is_author {
                        author.push_str(&txt);
                    } else {
                        date.push_str(&txt);
                    }
                }
            }
            Ok(Event::GeneralRef(r)) => {
                let target = match collecting {
                    Some(true) => &mut author,
                    Some(false) => &mut date,
                    None => continue,
                };
                target.extend(crate::resolve_general_ref(&r));
            }
            Ok(Event::End(e)) => match e.name().as_ref() {
                b"dc:creator" | b"dc:date" => collecting = None,
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    (author, date)
}

/// Render a comment's paragraphs as the body `text:p` sequence; only plain text + tabs/breaks are modeled.
fn render_comment_body(blocks: &[BlockView]) -> String {
    let mut out = String::new();
    for b in blocks {
        out.push_str("<text:p>");
        for c in b.text.chars() {
            match c {
                '\t' => out.push_str("<text:tab/>"),
                '\n' => out.push_str("<text:line-break/>"),
                _ => push_text(&mut out, c),
            }
        }
        out.push_str("</text:p>");
    }
    out
}

/// Opening `<office:annotation>` XML for a comment, with `office:name` tying it to the matching `annotation-end`.
pub(super) fn render_annotation_start(comment: &CommentView) -> String {
    let name = escape_attr(&comment.id);
    let date = if comment.date.is_empty() {
        String::new()
    } else {
        format!("<dc:date>{}</dc:date>", escape_text(&comment.date))
    };
    format!(
        r#"<office:annotation office:name="{name}"><dc:creator>{}</dc:creator>{date}{}</office:annotation>"#,
        escape_text(&comment.author),
        render_comment_body(&comment.blocks),
    )
}

/// Empty `<office:annotation-end/>` XML closing a comment's range; `office:name` matches the opening annotation's.
pub(super) fn render_annotation_end(comment: &CommentView) -> String {
    format!(
        r#"<office:annotation-end office:name="{}"/>"#,
        escape_attr(&comment.id)
    )
}
