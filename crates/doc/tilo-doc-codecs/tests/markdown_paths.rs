//! One Markdown for every published path: the document codec's import and export, a rich-text
//! value's markdown (what `RichText` and an agent's `r.markdown` read and write), and a body's
//! block lines (what an agent reads and writes with `read` and `replace-body`). Each drives a
//! realistic document through the interface its callers use, including the round trips.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_doc::Document;
use tilo_doc_codecs::blocks::{self, Ids, Shape};
use tilo_doc_codecs::rich;

/// A document of every kind Markdown carries: front matter, headings, marked paragraphs, a
/// task list, a numbered list, a quote, a callout, code, a table, a picture, a link with a space
/// in its address, and a footnote.
const DOC: &str = concat!(
    "---\n",
    "title: Plan\n",
    "lang: en\n",
    "---\n",
    "\n",
    "# Plan\n",
    "\n",
    "Text with **bold**, *em*, ~~struck~~, `code` and [the docs](https://example.test/a%20b).\n",
    "\n",
    "- [ ] oat milk\n",
    "- [x] eggs\n",
    "\n",
    "1. first\n",
    "2. next\n",
    "\n",
    "> a quote\n",
    "\n",
    "> [!WARNING]\n",
    "> mind the gap\n",
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
    "---\n",
    "\n",
    "A note.[^1]\n",
    "\n",
    "[^1]: The note.\n",
);

fn import(markdown: &str) -> Document {
    let (document, _) =
        tilo_doc_codecs::convert::import("md", markdown.as_bytes()).expect("the document reads");
    document
}

fn export(document: &Document) -> String {
    String::from_utf8(
        tilo_doc_codecs::convert::export(document, &tilo_content::NoContent, "md")
            .expect("the document writes"),
    )
    .expect("Markdown is UTF-8")
}

/// A body of every kind a block line carries: what `read` shows an agent and `replace-body`
/// takes back.
const BODY: &str = concat!(
    "# Plan\n",
    "\n",
    "Text with **bold**, *em*, ~~struck~~, `code` and [the docs](https://example.test).\n",
    "\n",
    "- [ ] oat milk\n",
    "- [x] eggs\n",
    "\n",
    "1. first\n",
    "\n",
    "> a quote\n",
    "\n",
    "> [!WARNING]\n",
    "> mind the gap\n",
    "\n",
    "```rust\n",
    "fn main() {}\n",
    "```\n",
    "\n",
    "---\n",
);

#[test]
fn every_path_reads_and_writes_the_same_document() {
    // The codec exports what it imported, and its export is its own fixed point (an export
    // copied again is the same file).
    let document = import(DOC);
    let first = export(&document);
    let second = export(&import(&first));
    assert_eq!(first, second, "export is stable under re-import: {first}");

    let views = document.as_ref().materialize().blocks;
    let kinds: Vec<&str> = views.iter().map(|view| view.kind.as_str()).collect();
    for kind in [
        "heading",
        "paragraph",
        "list_item",
        "quote",
        "divider",
        "cell",
    ] {
        assert!(kinds.contains(&kind), "{kind} reads as a block: {kinds:?}");
    }
    assert!(
        views
            .iter()
            .any(|view| view.str_attr("alert") == Some("warning")),
        "a callout keeps that it is one"
    );
    assert!(
        views
            .iter()
            .any(|view| view.bool_attr("checked") == Some(true)),
        "a done task keeps that it is done"
    );
    let link = views
        .iter()
        .flat_map(|view| &view.marks)
        .find(|mark| mark.name == "link")
        .expect("the link reads as a mark");
    assert_eq!(link.value.as_str(), Some("https://example.test/a%20b"));
}

#[test]
fn a_rich_text_value_reads_and_writes_the_markdown_it_holds() {
    // What a `RichText` field and `r.markdown` use: the value's blocks as the Markdown they
    // were read from, and that Markdown back as the same blocks.
    let source = "# Groceries\n\n- [ ] oat milk\n\nText with **bold** and `code`.\n\n```rust\nfn main() {}\n```\n\n![a fox](fox.png)\n\n> a quote\n";
    let blocks = rich::blocks_of_markdown(source).expect("the value reads");
    let written = rich::markdown_of_blocks(&blocks);
    assert_eq!(written, source, "a value writes the Markdown it read");
    let again = rich::blocks_of_markdown(&written).expect("the written value reads");
    assert_eq!(
        rich::markdown_of_blocks(&again),
        written,
        "the round trip is a fixed point"
    );
}

#[test]
fn an_agent_reads_and_writes_the_same_blocks_by_their_ids() {
    let views = import(BODY).as_ref().materialize().blocks;
    let ids: Vec<&str> = views.iter().map(|view| view.id.as_str()).collect();
    let shown = blocks::short_ids(ids.iter().copied());
    let read = blocks::render(&views, &shown, true);
    let known = |token: &str| blocks::resolve(ids.iter().copied(), token).ok();
    let parsed = blocks::parse(&read, &Ids::Known(&known)).expect("the read parses back");

    assert_eq!(parsed.len(), views.len(), "one line a block: {read}");
    for (line, view) in parsed.iter().zip(&views) {
        let blocks::Parsed::Block(item) = line else {
            panic!("{line:?} for a block: {read}")
        };
        let shape = Shape::of_view(view).expect("a line of its own");
        assert_eq!(item.id.as_deref(), Some(view.id.as_str()), "{read}");
        assert_eq!(
            item.key(),
            blocks::line_key(&shape, &view.text, &view.marks),
            "{read}"
        );
    }

    // What an agent writes back with the same lines, no ids, is the same body.
    let written = blocks::parse(&blocks::render(&views, &shown, false), &Ids::Discard)
        .expect("the written lines parse");
    assert_eq!(written.len(), views.len());
    for (line, view) in written.iter().zip(&views) {
        let blocks::Parsed::Block(item) = line else {
            panic!("{line:?} for a block")
        };
        let shape = Shape::of_view(view).expect("a line of its own");
        assert_eq!(item.id, None, "a written line names no block");
        assert_eq!(
            item.key(),
            blocks::line_key(&shape, &view.text, &view.marks)
        );
    }
}

#[test]
fn text_that_reads_as_structure_stays_text_on_every_path() {
    // Paragraphs a person typed, a document as the editor stores them.
    let mut document = Document::new();
    {
        let mut dm = document.as_mut();
        dm.bulk(|dm| {
            let mut previous = None;
            for line in ["- not a list", "2. not numbered", "# not a heading"] {
                let block = dm.insert_block(previous.as_deref(), "paragraph");
                dm.splice_text(&block, 0, 0, line);
                previous = Some(block);
            }
        });
    }
    let written = export(&document);
    assert_eq!(
        written, "\\- not a list\n\n2\\. not numbered\n\n\\# not a heading\n",
        "the export escapes what would read as a marker"
    );
    let back = import(&written);
    let views = back.as_ref().materialize().blocks;
    assert_eq!(
        views
            .iter()
            .map(|view| view.kind.as_str())
            .collect::<Vec<_>>(),
        ["paragraph", "paragraph", "paragraph"],
        "the words stay words"
    );
    assert_eq!(
        views
            .iter()
            .map(|view| view.text.as_str())
            .collect::<Vec<_>>(),
        ["- not a list", "2. not numbered", "# not a heading"]
    );

    // The body's block lines read and write the same words.
    let read = blocks::render(&views, &std::collections::BTreeMap::new(), false);
    assert!(
        read.contains("\\- not a list") && read.contains("2\\. not numbered"),
        "{read}"
    );
    let parsed = blocks::parse(&read, &Ids::Discard).expect("the lines read");
    let texts: Vec<String> = parsed
        .into_iter()
        .map(|line| match line {
            blocks::Parsed::Block(item) => item.text,
            blocks::Parsed::Keep(id) => panic!("kept {id}"),
        })
        .collect();
    assert_eq!(
        texts,
        ["- not a list", "2. not numbered", "# not a heading"]
    );
}
