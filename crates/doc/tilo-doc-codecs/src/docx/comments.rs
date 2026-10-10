//! Comments / annotations ⇄ OOXML `word/comments.xml`: `<w:comment w:id w:author w:date>`
//! elements of `w:p` paragraphs, referenced from the body by `<w:commentRangeStart/End
//! w:id="N"/>` bracketing the range and a `<w:commentReference w:id="N"/>` run. This maps
//! a comment onto a `comment` mark over the body range carrying the comment id, plus its
//! author/date/content in the `comments` container.

use super::side_part;
use super::styles::attr_val;
use crate::xml_escape::escape_attr;
use std::collections::BTreeMap;
use tilo_doc::model::{DocMut, DocRef, materialize_comments};
// `write!` into a `String` cannot fail; the discarded `Result` is the
// `fmt::Write` contract, not a swallowed error.
use std::fmt::Write as _;

/// The fixed part name + relationship id qdoc writes for comments.
pub(super) const COMMENTS_PART: &str = "word/comments.xml";

/// Import `word/comments.xml`, returning a `docx_id → qdoc_comment_id` map so the body
/// parser can wire each `commentRangeStart/End` onto the canonical comment. Each `w:p` of
/// a comment becomes a paragraph block.
pub(super) fn import_comments_part(xml: &[u8], doc: &mut DocMut) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    side_part::scan_items(
        xml,
        b"w:comment",
        |e| {
            let id = attr_val(e, b"w:id")?;
            let author = attr_val(e, b"w:author").unwrap_or_default();
            let date = attr_val(e, b"w:date").unwrap_or_default();
            Some((id, author, date))
        },
        |(id, author, date), raw| {
            let comment_id = doc.insert_comment(&author, &date);
            side_part::import_paragraphs(
                raw,
                doc,
                |doc, prev| doc.insert_comment_block(&comment_id, prev, "paragraph"),
                |doc, block, at, text| doc.splice_comment_text(&comment_id, block, at, 0, text),
            );
            map.insert(id, comment_id);
        },
    );
    map
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
            side_part::render_paragraphs(&c.blocks),
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
