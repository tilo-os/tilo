//! A Markdown document as the blocks a rich-text field holds, and back: a codec's Markdown is
//! imported into a scratch document and read as its blocks, and blocks are exported as Markdown.
//!
//! Two blocks are stored the way a view draws them: a picture is the Markdown it is written as,
//! `![alt](source)`, in a paragraph of its own, and a table is one paragraph of its Markdown
//! source marked with [`TABLE_SOURCE`].

use std::collections::BTreeMap;

use tilo_doc::snapshot::{AttrVal, BlockView};

use crate::format::CodecError;

pub const TABLE_SOURCE: &str = "table_source";

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

#[must_use]
pub fn markdown_of_blocks(blocks: &[BlockView]) -> String {
    String::from_utf8_lossy(&crate::markdown::export_views(
        &BTreeMap::new(),
        blocks,
        &[],
    ))
    .into_owned()
}

/// `blocks` as a document stores them: a run of table cells gathered into the one paragraph a
/// table is kept as, and every other block as it is.
fn editor_blocks(blocks: &[BlockView]) -> Vec<BlockView> {
    let mut out = Vec::with_capacity(blocks.len());
    let mut index = 0;
    while let Some(block) = blocks.get(index) {
        let table = table_of(block);
        let Some(table) = table else {
            out.push(block.clone());
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

    #[test]
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

    #[test]
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
}
