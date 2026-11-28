//! A Markdown document as the blocks a rich-text field holds, and back: a codec's Markdown is
//! imported into a scratch document and read as its blocks, and blocks are exported as Markdown.
//!
//! Two blocks are stored the way a view draws them: a picture is the Markdown it is written as,
//! `![alt](source)`, in a paragraph of its own, a table is one paragraph of its Markdown
//! source marked with [`TABLE_SOURCE`], and a table of contents is `[TOC]`, stored as the field
//! block a layout fills with the document's headings.

use std::collections::BTreeMap;

use tilo_doc::document::{MarkSpan, MarkValue};
use tilo_doc::snapshot::{AttrVal, BlockView};

use crate::format::CodecError;

pub const TABLE_SOURCE: &str = tilo_doc::table_source::ATTR;

/// The line a table of contents is written as, in Markdown: what a person types and what a
/// document written out carries, in the one form every Markdown reader keeps as text.
pub use tilo_doc::schema::TOC_LINE;

/// A Markdown document as the blocks a rich-text field holds: headings, paragraphs, lists,
/// checklists, quotes, callouts, code (a formula is a fenced block of its own language), a
/// divider, tables, pictures and the inline marks Markdown carries.
///
/// # Errors
///
/// Why the text is not a document: more than [`crate::convert::MAX_DOCUMENT_BYTES`].
pub fn blocks_of_markdown(markdown: &str) -> Result<Vec<BlockView>, CodecError> {
    let (document, _) = crate::convert::import("md", markdown.as_bytes())?;
    Ok(editor_blocks(&document.as_ref().materialize().blocks))
}

/// An HTML document as the blocks a rich-text field holds, read by the one HTML importer
/// ([`crate::html`]): a book's chapter, a saved page and pasted markup all read the same way.
///
/// # Errors
///
/// Why the text is not a document: more than [`crate::convert::MAX_DOCUMENT_BYTES`].
#[cfg(feature = "html")]
pub fn blocks_of_html(html: &str) -> Result<Vec<BlockView>, CodecError> {
    let (document, _) = crate::convert::import("html", html.as_bytes())?;
    Ok(editor_blocks(&document.as_ref().materialize().blocks))
}

/// A document file (Word, `OpenDocument`, RTF, HTML, Markdown or plain text) as the blocks a
/// rich-text field holds, with what the file carries about its words: a comment is a `comment`
/// mark over the words it is about, valued with a [`CommentStamp`]; a tracked change is an `ins`
/// or `del` mark valued with a [`ChangeStamp`]. The editor keeps both when it edits the words, so
/// [`document_of_blocks`] writes them back out.
///
/// # Errors
///
/// Why the file is not a document: a format this build has no codec for, or more than
/// [`crate::convert::MAX_DOCUMENT_BYTES`].
pub fn blocks_of_document(ext: &str, bytes: &[u8]) -> Result<Vec<BlockView>, CodecError> {
    let (document, _) = crate::convert::import(ext, bytes)?;
    let snapshot = document.as_ref().materialize();
    let mut blocks = editor_blocks(&snapshot.blocks);
    if crate::convert::reading(ext) == crate::convert::Reading::Text {
        // Plain text is one paragraph a line, as a word processor reads one: a blank line ends
        // a paragraph rather than standing as an empty one.
        blocks.retain(|block| {
            !(block.kind == "paragraph"
                && block.text.trim().is_empty()
                && block.marks.is_empty()
                && block.children.is_empty())
        });
    }
    stamp_marks(&mut blocks, &snapshot);
    Ok(blocks)
}

/// The comments `bytes` carry, as JSON: each one's author, when it was written, what it says, and
/// the words of the body it is about — the paragraph and the range in it, or nothing when the
/// text it commented on is gone. What a view lists beside a document a file brought with it.
///
/// # Errors
///
/// Why the file is not a document: a format this build has no codec for, or more than
/// [`crate::convert::MAX_DOCUMENT_BYTES`].
pub fn comments_json(ext: &str, bytes: &[u8]) -> Result<String, CodecError> {
    let (document, _) = crate::convert::import(ext, bytes)?;
    let snapshot = document.as_ref().materialize();
    let comments: Vec<serde_json::Value> = snapshot
        .comments
        .iter()
        .map(|comment| {
            let (block, start, end) = comment
                .range
                .as_ref()
                .map_or((String::new(), 0, 0), |(block, start, end)| {
                    (block.clone(), *start, *end)
                });
            serde_json::json!({
                "author": comment.author,
                "date": comment.date,
                "text": comment
                    .blocks
                    .iter()
                    .map(|block| block.text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
                "block": block,
                "start": start,
                "end": end,
            })
        })
        .collect();
    serde_json::to_string(&comments).map_err(|error| CodecError::Export(error.to_string()))
}

/// Blocks as a file of `ext`: a document format (Word, `OpenDocument`, RTF, HTML) written from
/// the words and the comment and change marks they carry, or Markdown and plain text.
///
/// A comment mark whose value is not a [`CommentStamp`] — one the app made in this document, whose
/// words live in the workspace — is written out as the plain words it covers: what is a resource
/// here has no place in a file another writer opens.
///
/// # Errors
///
/// Why the blocks cannot be written: a format this build has no codec for, or codec failures.
pub fn document_of_blocks(blocks: &[BlockView], ext: &str) -> Result<Vec<u8>, CodecError> {
    let document = document_from_blocks(blocks)?;
    #[cfg(feature = "epub")]
    if ext.eq_ignore_ascii_case("epub") {
        return crate::export_epub::render_epub(
            &document.as_ref().materialize(),
            &tilo_content::NoContent,
        )
        .map_err(|why| CodecError::Export(why.to_string()));
    }
    crate::convert::export(&document, &tilo_content::NoContent, ext)
}

/// The document `blocks` say, with the comments and tracked changes their marks carry, as the
/// format codecs write: a file of one of those formats, a PDF rendered from it, or what a caller
/// reads of it before writing.
///
/// # Errors
///
/// Why the blocks cannot be a document: a table's source is not a table, or a mark's stamp is not
/// one the export can write.
pub fn document_from_blocks(blocks: &[BlockView]) -> Result<tilo_doc::Document, CodecError> {
    let expanded = expand_tables(blocks)?;
    let mut document = tilo_doc::Document::new();
    let written = {
        let mut dm = document.as_mut();
        let marks = register_marks(&mut dm, &expanded);
        marks.applied(&expanded)
    };
    {
        let mut dm = document.as_mut();
        tilo_doc::reconcile::reconcile_body(&mut dm, &written);
    }
    Ok(document)
}

/// One comment as a document body carries it: its author, when it was written, what it says and
/// the answers under it. A comment mark's value is this as JSON, so the body alone holds
/// everything a comment needs to be written back into a file.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CommentStamp {
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub date: String,
    #[serde(default)]
    pub blocks: Vec<BlockView>,
    #[serde(default)]
    pub replies: Vec<ReplyStamp>,
}

/// One answer under a comment, as a [`CommentStamp`] carries it.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReplyStamp {
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub date: String,
    #[serde(default)]
    pub blocks: Vec<BlockView>,
}

/// One tracked change as a document body carries it: who made it and when. Whether it inserted
/// or deleted words is the mark's own name (`ins` or `del`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ChangeStamp {
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub date: String,
}

/// The comment a mark's value stamps, when it is one this carried in rather than a resource id.
#[must_use]
pub fn comment_stamp(value: &str) -> Option<CommentStamp> {
    serde_json::from_str(value).ok()
}

/// The change a mark's value stamps, when it is one this carried in.
#[must_use]
pub fn change_stamp(value: &str) -> Option<ChangeStamp> {
    serde_json::from_str(value).ok()
}

/// Replace each comment and change mark's file id with the stamp of what the file says about it.
fn stamp_marks(blocks: &mut [BlockView], snapshot: &tilo_doc::snapshot::DocSnapshot) {
    use tilo_doc::schema::{MARK_COMMENT, MARK_DEL, MARK_INS};
    let comments: BTreeMap<&str, &tilo_doc::snapshot::CommentView> = snapshot
        .comments
        .iter()
        .map(|comment| (comment.id.as_str(), comment))
        .collect();
    let changes: BTreeMap<&str, &tilo_doc::snapshot::ChangeView> = snapshot
        .changes
        .iter()
        .map(|change| (change.id.as_str(), change))
        .collect();
    for block in blocks {
        for mark in &mut block.marks {
            let Some(id) = mark.value.as_str() else {
                continue;
            };
            if mark.name == MARK_COMMENT {
                let Some(comment) = comments.get(id) else {
                    continue;
                };
                let stamp = CommentStamp {
                    author: comment.author.clone(),
                    date: comment.date.clone(),
                    blocks: comment.blocks.clone(),
                    replies: comment
                        .replies
                        .iter()
                        .map(|reply| ReplyStamp {
                            author: reply.author.clone(),
                            date: reply.date.clone(),
                            blocks: reply.blocks.clone(),
                        })
                        .collect(),
                };
                if let Ok(json) = serde_json::to_string(&stamp) {
                    mark.value = MarkValue::Str(json);
                }
            } else if mark.name == MARK_INS || mark.name == MARK_DEL {
                let Some(change) = changes.get(id) else {
                    continue;
                };
                let stamp = ChangeStamp {
                    author: change.author.clone(),
                    date: change.date.clone(),
                };
                if let Ok(json) = serde_json::to_string(&stamp) {
                    mark.value = MarkValue::Str(json);
                }
            }
        }
    }
}

/// The comments and changes the marks name, as containers a document holds, with each mark's value
/// replaced by the id the container gave it.
struct Registered {
    comments: BTreeMap<String, String>,
    changes: BTreeMap<String, String>,
}

impl Registered {
    /// `blocks` with each stamp mark valued by the id its comment or change was kept under.
    fn applied(&self, blocks: &[BlockView]) -> Vec<BlockView> {
        let mut out = blocks.to_vec();
        for block in &mut out {
            for mark in &mut block.marks {
                let Some(value) = mark.value.as_str() else {
                    continue;
                };
                // An insertion and a deletion made by one person at one moment stamp the same
                // words, and are two changes: the mark's own name tells them apart.
                let kept = self
                    .comments
                    .get(value)
                    .or_else(|| self.changes.get(&change_key(&mark.name, value)));
                if let Some(id) = kept {
                    mark.value = MarkValue::Str(id.clone());
                }
            }
        }
        out
    }
}

/// The key a change mark is registered under: its kind and what the file said of it, so an
/// insertion and a deletion of one person at one moment are two changes.
fn change_key(name: &str, value: &str) -> String {
    format!("{name}\u{1}{value}")
}

fn register_marks(dm: &mut tilo_doc::model::DocMut<'_>, blocks: &[BlockView]) -> Registered {
    use tilo_doc::schema::{MARK_COMMENT, MARK_DEL, MARK_INS};
    let mut registered = Registered {
        comments: BTreeMap::new(),
        changes: BTreeMap::new(),
    };
    for block in blocks {
        for mark in &block.marks {
            let Some(value) = mark.value.as_str() else {
                continue;
            };
            if mark.name == MARK_COMMENT {
                if registered.comments.contains_key(value) {
                    continue;
                }
                let Some(stamp) = comment_stamp(value) else {
                    continue;
                };
                let id = dm.insert_comment(&stamp.author, &stamp.date);
                write_comment_body(dm, &id, &stamp.blocks);
                for reply in &stamp.replies {
                    let Some(reply_id) = dm.insert_comment_reply(&id, &reply.author, &reply.date)
                    else {
                        continue;
                    };
                    write_reply_body(dm, &id, &reply_id, &reply.blocks);
                }
                registered.comments.insert(value.to_owned(), id);
            } else if mark.name == MARK_INS || mark.name == MARK_DEL {
                let key = change_key(&mark.name, value);
                if registered.changes.contains_key(&key) {
                    continue;
                }
                let Some(stamp) = change_stamp(value) else {
                    continue;
                };
                let kind = if mark.name == MARK_DEL {
                    tilo_doc::snapshot::ChangeKind::Del
                } else {
                    tilo_doc::snapshot::ChangeKind::Ins
                };
                let id = dm.insert_change(kind, &stamp.author, &stamp.date);
                registered.changes.insert(key, id);
            }
        }
    }
    registered
}

/// The blocks of a comment's own words, written under the comment `id`.
fn write_comment_body(dm: &mut tilo_doc::model::DocMut<'_>, id: &str, blocks: &[BlockView]) {
    let mut previous: Option<String> = None;
    for block in blocks {
        if let Some(block_id) = dm.insert_comment_block(id, previous.as_deref(), &block.kind) {
            dm.splice_comment_text(id, &block_id, 0, 0, &block.text);
            previous = Some(block_id);
        }
    }
}

/// The blocks of a reply's own words, written under the reply `reply` of the comment `id`.
fn write_reply_body(
    dm: &mut tilo_doc::model::DocMut<'_>,
    id: &str,
    reply: &str,
    blocks: &[BlockView],
) {
    let mut previous: Option<String> = None;
    for block in blocks {
        if let Some(block_id) =
            dm.insert_comment_reply_block(id, reply, previous.as_deref(), &block.kind)
        {
            dm.splice_comment_reply_text(id, reply, &block_id, 0, 0, &block.text);
            previous = Some(block_id);
        }
    }
}

/// `blocks` with each table's stored Markdown source replaced by the cells it says: what a
/// document format writes a table from.
fn expand_tables(blocks: &[BlockView]) -> Result<Vec<BlockView>, CodecError> {
    let mut out = Vec::with_capacity(blocks.len());
    for block in blocks {
        if !block.attrs.contains_key(TABLE_SOURCE) {
            out.push(block.clone());
            continue;
        }
        let (table, _) = crate::convert::import("md", block.text.as_bytes())?;
        let cells: Vec<BlockView> = table
            .as_ref()
            .materialize()
            .blocks
            .into_iter()
            .filter(|view| view.kind == "cell")
            .collect();
        if cells.is_empty() {
            out.push(block.clone());
            continue;
        }
        // Each table's cells and its table id are named apart from every other table's, so two
        // tables do not merge when the document takes them.
        let stem = if block.id.is_empty() {
            format!("table{}", out.len())
        } else {
            block.id.clone()
        };
        let table_id = format!("{stem}.table");
        for (index, mut cell) in cells.into_iter().enumerate() {
            cell.id = format!("{stem}.{index}");
            if cell.attrs.contains_key("table") {
                cell.attrs
                    .insert("table".to_owned(), AttrVal::Str(table_id.clone()));
            }
            out.push(cell);
        }
    }
    Ok(out)
}

#[must_use]
pub fn markdown_of_blocks(blocks: &[BlockView]) -> String {
    let written: Vec<BlockView> = blocks.iter().map(written_block).collect();
    String::from_utf8_lossy(&crate::markdown::export_views(
        &BTreeMap::new(),
        &written,
        &[],
    ))
    .into_owned()
}

/// `blocks` as a document stores them: a run of table cells gathered into the one paragraph a
/// table is kept as, a `[TOC]` line the field block a layout fills, and every other block as it
/// is.
fn editor_blocks(blocks: &[BlockView]) -> Vec<BlockView> {
    let mut out = Vec::with_capacity(blocks.len());
    let mut index = 0;
    while let Some(block) = blocks.get(index) {
        let table = table_of(block);
        let Some(table) = table else {
            out.push(if is_toc_line(block) {
                toc_block(block)
            } else {
                block.clone()
            });
            index = index.saturating_add(1);
            continue;
        };
        let start = index;
        while blocks
            .get(index)
            .is_some_and(|cell| table_of(cell).as_deref() == Some(table.as_str()))
        {
            index = index.saturating_add(1);
        }
        let cells = blocks.get(start..index).unwrap_or_default();
        out.push(table_paragraph(cells));
    }
    out
}

/// Whether `block` is a paragraph whose whole text is a table of contents line.
fn is_toc_line(block: &BlockView) -> bool {
    block.kind == "paragraph" && block.text.trim().eq_ignore_ascii_case(TOC_LINE)
}

/// The field block a table of contents is kept as: one placeholder character, marked with the
/// field a layout draws the headings into. It keeps the paragraph's id, so a document that keeps
/// it keeps the block where a person put it.
fn toc_block(block: &BlockView) -> BlockView {
    BlockView {
        id: block.id.clone(),
        kind: "paragraph".to_owned(),
        text: tilo_doc::schema::FIELD_PLACEHOLDER.to_string(),
        marks: vec![MarkSpan {
            start: 0,
            end: 1,
            name: tilo_doc::schema::MARK_FIELD.to_owned(),
            value: MarkValue::Str(tilo_doc::schema::FIELD_TOC.to_owned()),
        }],
        attrs: block.attrs.clone(),
        children: Vec::new(),
    }
}

/// Whether `block` is a table of contents field: the one placeholder character under the field
/// mark that names it.
#[must_use]
pub fn is_toc(block: &BlockView) -> bool {
    block.text == tilo_doc::schema::FIELD_PLACEHOLDER.to_string()
        && block.marks.iter().any(|mark| {
            mark.name == tilo_doc::schema::MARK_FIELD
                && mark.value.as_str() == Some(tilo_doc::schema::FIELD_TOC)
        })
}

/// `block` as it is written out as Markdown: a table of contents field as the `[TOC]` line.
fn written_block(block: &BlockView) -> BlockView {
    if !is_toc(block) {
        return block.clone();
    }
    let mut written = block.clone();
    TOC_LINE.clone_into(&mut written.text);
    written.marks.clear();
    written
}

fn table_of(block: &BlockView) -> Option<String> {
    (block.kind == "cell").then(|| block.str_attr("table").unwrap_or_default().to_owned())
}

/// `cells`, one table's cells in order, as the paragraph a table is kept as: its first cell names
/// the block, so the table keeps the identity the document gave it.
fn table_paragraph(cells: &[BlockView]) -> BlockView {
    let source =
        String::from_utf8_lossy(&crate::markdown::export_views(&BTreeMap::new(), cells, &[]))
            .trim_end()
            .to_owned();
    BlockView {
        id: cells
            .first()
            .map(|cell| cell.id.clone())
            .unwrap_or_default(),
        kind: "paragraph".to_owned(),
        text: source,
        marks: Vec::new(),
        attrs: BTreeMap::from([(TABLE_SOURCE.to_owned(), AttrVal::Bool(true))]),
        children: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document() -> Vec<BlockView> {
        blocks_of_markdown(
            "# Groceries\n\n- [ ] oat milk\n- [x] eggs\n\nText with **bold**, `code` and \
             [a link](https://example.test).\n\n```rust\nfn main() {}\n```\n\n| a | b |\n| --- | \
             --- |\n| 1 | 2 |\n\n![a fox](fox.png)\n\n> a quote\n\n---\n",
        )
        .expect("Markdown reads")
    }

    fn a_document_reads_as_the_blocks_an_editor_draws() {
        let blocks = document();
        let kinds: Vec<&str> = blocks.iter().map(|block| block.kind.as_str()).collect();
        assert_eq!(
            kinds,
            [
                "heading",
                "list_item",
                "list_item",
                "paragraph",
                "paragraph",
                "paragraph",
                "paragraph",
                "quote",
                "divider",
            ],
            "a fence, a table and a picture are paragraphs: {kinds:?}"
        );
        assert_eq!(blocks[1].bool_attr("checked"), Some(false));
        assert_eq!(blocks[2].bool_attr("checked"), Some(true));
        assert!(
            blocks[3].marks.iter().any(|mark| mark.name == "strong"
                && mark.value == tilo_doc::document::MarkValue::Bool(true)),
            "bold is a mark: {:?}",
            blocks[3].marks
        );
        assert!(
            blocks[3]
                .marks
                .iter()
                .any(|mark| mark.name == "link"
                    && mark.value.as_str() == Some("https://example.test")),
            "a link is a mark: {:?}",
            blocks[3].marks
        );
        assert_eq!(blocks[4].bool_attr("code"), Some(true));
        assert!(blocks[5].flag(TABLE_SOURCE));
        assert!(
            blocks[5].text.starts_with("| a | b |"),
            "{}",
            blocks[5].text
        );
        assert_eq!(blocks[6].text, "![a fox](fox.png)");
    }

    fn a_documents_blocks_write_as_the_markdown_they_were_read_from() {
        let markdown = concat!(
            "# Groceries\n",
            "\n",
            "- [ ] oat milk\n",
            "- [x] eggs\n",
            "\n",
            "Text with **bold**, `code` and [a link](https://example.test).\n",
            "\n",
            "```rust\n",
            "fn main() {}\n",
            "```\n",
            "\n",
            "| a | b |\n",
            "| --- | --- |\n",
            "| 1 | 2 |\n",
            "\n",
            "![a fox](fox.png)\n",
            "\n",
            "> a quote\n",
            "\n",
            "---\n",
        );
        assert_eq!(markdown_of_blocks(&document()), markdown);
        assert_eq!(
            markdown_of_blocks(&blocks_of_markdown(markdown).expect("Markdown reads")),
            markdown,
            "a round trip keeps the document"
        );
    }

    /// A Word file with a comment over its words and an inserted and a deleted run, as one a
    /// colleague sends: what the app opens and writes back.
    fn commented_docx() -> Vec<u8> {
        let mut document = tilo_doc::Document::new();
        {
            let mut dm = document.as_mut();
            let first = dm.insert_block(None, "paragraph");
            dm.splice_text(&first, 0, 0, "The plan holds. We ship on Monday.");
            let comment = dm.insert_comment("Ada Lovelace", "2026-03-02T10:00:00Z");
            if let Some(words) = dm.insert_comment_block(&comment, None, "paragraph") {
                dm.splice_comment_text(&comment, &words, 0, 0, "The plan holds a date.");
            }
            dm.add_comment_mark(&first, 4, 8, &comment);
            let second = dm.insert_block(Some(&first), "paragraph");
            dm.splice_text(&second, 0, 0, "We ship on Tuesday.");
            let inserted = dm.insert_change(
                tilo_doc::snapshot::ChangeKind::Ins,
                "Grace Hopper",
                "2026-03-02T11:00:00Z",
            );
            dm.add_change_mark(
                tilo_doc::snapshot::ChangeKind::Ins,
                &second,
                3,
                7,
                &inserted,
            );
            let deleted = dm.insert_change(
                tilo_doc::snapshot::ChangeKind::Del,
                "Grace Hopper",
                "2026-03-02T11:00:00Z",
            );
            dm.add_change_mark(
                tilo_doc::snapshot::ChangeKind::Del,
                &second,
                11,
                18,
                &deleted,
            );
        }
        crate::convert::export(&document, &tilo_content::NoContent, "docx").expect("a Word file")
    }

    #[test]
    fn a_word_files_comments_and_changes_round_trip_through_the_editors_blocks() {
        let bytes = commented_docx();
        let blocks = blocks_of_document("docx", &bytes).expect("the Word file reads");
        let comment = blocks
            .iter()
            .flat_map(|block| block.marks.iter())
            .find(|mark| mark.name == tilo_doc::schema::MARK_COMMENT)
            .and_then(|mark| mark.value.as_str())
            .and_then(comment_stamp)
            .expect("the comment is a stamp");
        assert_eq!(comment.author, "Ada Lovelace");
        assert_eq!(comment.date, "2026-03-02T10:00:00Z");
        assert!(
            comment
                .blocks
                .iter()
                .any(|block| block.text.contains("The plan holds")),
            "the comment's own words travel: {comment:?}"
        );
        let stamps: Vec<(String, ChangeStamp)> = blocks
            .iter()
            .flat_map(|block| block.marks.iter())
            .filter(|mark| mark.name == "ins" || mark.name == "del")
            .map(|mark| {
                (
                    mark.name.clone(),
                    change_stamp(mark.value.as_str().expect("a stamped change"))
                        .expect("the change is a stamp"),
                )
            })
            .collect();
        assert_eq!(stamps.len(), 2, "{stamps:?}");
        assert!(
            stamps
                .iter()
                .all(|(_, stamp)| stamp.author == "Grace Hopper"),
            "the changes keep their author: {stamps:?}"
        );

        // Edited, then written back: the file still holds the comment and both changes.
        let written = document_of_blocks(&blocks, "docx").expect("a Word file");
        let reopened = blocks_of_document("docx", &written).expect("the saved file reads");
        let comment = reopened
            .iter()
            .flat_map(|block| block.marks.iter())
            .find_map(|mark| {
                (mark.name == tilo_doc::schema::MARK_COMMENT)
                    .then(|| mark.value.as_str())
                    .flatten()
                    .and_then(comment_stamp)
            })
            .expect("the comment came back");
        assert_eq!(comment.author, "Ada Lovelace");
        let changes: Vec<String> = reopened
            .iter()
            .flat_map(|block| block.marks.iter())
            .filter(|mark| mark.name == "ins" || mark.name == "del")
            .filter_map(|mark| mark.value.as_str().and_then(change_stamp))
            .map(|stamp| format!("{} {}", stamp.author, stamp.date))
            .collect();
        assert_eq!(changes.len(), 2, "{changes:?}");
        assert!(
            reopened.iter().any(|block| block.text.contains("Tuesday")),
            "the words stay: {reopened:?}"
        );
    }

    #[test]
    fn a_document_reads_as_blocks_and_writes_back_as_its_markdown() {
        let cases: [(&str, fn()); 3] = [
            (
                "a document reads as the blocks an editor draws",
                a_document_reads_as_the_blocks_an_editor_draws,
            ),
            (
                "a documents blocks write as the markdown they were read from",
                a_documents_blocks_write_as_the_markdown_they_were_read_from,
            ),
            (
                "a word files comments and changes round trip through the editors blocks",
                a_word_files_comments_and_changes_round_trip_through_the_editors_blocks,
            ),
        ];
        for (case, run) in cases {
            eprintln!("case: {case}");
            run();
        }
    }
}
