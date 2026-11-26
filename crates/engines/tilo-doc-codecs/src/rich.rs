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
