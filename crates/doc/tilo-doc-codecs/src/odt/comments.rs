//! Comments ⇄ ODF `office:annotation`. ODF places a comment inline at the start of its
//! range as `<office:annotation office:name="…">` (author/date/body); a ranged comment
//! also emits `<office:annotation-end office:name="…"/>` at the range end, tied to the
//! start via the shared `office:name`. This maps onto a `comment` mark over the body range.

use super::plain_paragraphs;
use crate::xml_escape::{escape_attr, escape_text};
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use tilo_doc::model::DocMut;
use tilo_doc::snapshot::CommentView;

pub(super) fn import_annotation(raw: &str, doc: &mut DocMut) -> (Option<String>, String) {
    let name = annotation_name(raw);
    let (author, date) = annotation_meta(raw);
    let comment_id = doc.insert_comment(&author, &date);
    plain_paragraphs::import(
        raw,
        None,
        doc,
        |doc, prev| doc.insert_comment_block(&comment_id, prev, "paragraph"),
        |doc, block, at, text| doc.splice_comment_text(&comment_id, block, at, 0, text),
        |_, _, _| None,
    );
    (name, comment_id)
}

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
        plain_paragraphs::render(&comment.blocks),
    )
}

pub(super) fn render_annotation_end(comment: &CommentView) -> String {
    format!(
        r#"<office:annotation-end office:name="{}"/>"#,
        escape_attr(&comment.id)
    )
}
