//! Markdown typed into a note becomes formatting as its closing delimiter is
//! typed, stored in the notebook record and kept on reopening.

use crate::framework_support::with_note;
use notes::{PageId, markdown, read};
use ui::framework::testing::TestApp;
use ui::widget::Block;

type App = TestApp<notes::NotesShell>;

/// Click into the open note's body and type `text` key by key.
fn type_in_body(app: &mut App, text: &str) {
    let editor = *app.harness().targets().get("block_editor").unwrap();
    app.harness().click_at(editor.x0 + 20.0, editor.y0 + 10.0);
    app.harness().type_text(text);
}

/// The page as the notebook stores it, written out as Markdown.
fn stored_markdown(app: &App, page: PageId) -> String {
    let blocks: Vec<Block> = app
        .read_document(|doc| read::page_blocks(doc, page))
        .into_iter()
        .map(Block::from_content)
        .collect();
    markdown::serialize(&markdown::blocks_to_specs(&blocks))
}

#[test]
fn typed_markdown_is_stored_as_formatting_and_survives_a_reopen() {
    let (mut app, page) = with_note("Typed markdown", "");
    type_in_body(&mut app, "*hi* and **b** and `c` and ~~s~~ end");
    assert_eq!(
        app.read_document(|doc| read::page_blocks(doc, page))[0].text,
        "hi and b and c and s end",
        "the delimiters leave the stored text"
    );
    let expected = "*hi* and **b** and `c` and ~~s~~ end\n";
    assert_eq!(stored_markdown(&app, page), expected);
    let app = app.reopen().unwrap();
    assert_eq!(stored_markdown(&app, page), expected);
}

#[test]
fn delimiters_that_close_nothing_stay_as_typed() {
    let (mut app, page) = with_note("Literal", "");
    type_in_body(&mut app, "snake_case_name and 2 * 3 * 4");
    let blocks = app.read_document(|doc| read::page_blocks(doc, page));
    assert_eq!(blocks[0].text, "snake_case_name and 2 * 3 * 4");
    assert!(blocks[0].marks.is_empty(), "{:?}", blocks[0].marks);
}

#[test]
fn undo_or_backspace_after_formatting_brings_the_markdown_back() {
    let (mut app, page) = with_note("Undo markdown", "");
    type_in_body(&mut app, "a **b**");
    assert_eq!(stored_markdown(&app, page), "a **b**\n");
    app.harness().key("Backspace");
    let blocks = app.read_document(|doc| read::page_blocks(doc, page));
    assert_eq!(
        blocks[0].text, "a **b**",
        "Backspace puts the markdown back"
    );
    assert!(blocks[0].marks.is_empty(), "{:?}", blocks[0].marks);

    let (mut app, page) = with_note("Undo markdown", "");
    type_in_body(&mut app, "a **b**");
    assert!(app.undo());
    let blocks = app.read_document(|doc| read::page_blocks(doc, page));
    assert!(
        blocks[0].text.starts_with("a **b*") && blocks[0].marks.is_empty(),
        "undo brings the delimiters back unformatted: {blocks:?}"
    );
}
