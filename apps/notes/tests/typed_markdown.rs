//! Markdown typed into a note becomes formatting as its closing delimiter is
//! typed, and contractions typed without an apostrophe get one back at the
//! word's end: both stored in the notebook record and kept on reopening.

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

    // The formatting is an undo step of its own after the keystroke that
    // asked for it: undo gives back exactly what was typed, caret after it,
    // and the next undo takes back the typing (a word at a time).
    let (mut app, page) = with_note("Undo markdown", "");
    type_in_body(&mut app, "a **b**");
    assert!(app.undo());
    let blocks = app.read_document(|doc| read::page_blocks(doc, page));
    assert_eq!(blocks[0].text, "a **b**", "{blocks:?}");
    assert!(blocks[0].marks.is_empty(), "{:?}", blocks[0].marks);
    app.harness().key("x");
    let blocks = app.read_document(|doc| read::page_blocks(doc, page));
    assert_eq!(
        blocks[0].text, "a **b**x",
        "the caret is back after what was typed"
    );
    assert!(blocks[0].marks.is_empty(), "{:?}", blocks[0].marks);

    let (mut app, page) = with_note("Undo typing", "");
    type_in_body(&mut app, "a **b**");
    assert!(app.undo());
    assert!(app.undo());
    assert_eq!(
        app.read_document(|doc| read::page_blocks(doc, page))[0].text,
        "a "
    );
}

#[test]
fn undo_after_an_autocorrection_gives_back_the_word_as_typed() {
    let (mut app, page) = with_note("Undo correction", "");
    type_in_body(&mut app, "it doesnt ");
    let text = |app: &App| {
        app.read_document(|doc| read::page_blocks(doc, page))[0]
            .text
            .clone()
    };
    assert_eq!(text(&app), "it doesn't ");
    assert!(app.undo());
    assert_eq!(
        text(&app),
        "it doesnt ",
        "the correction is its own undo step"
    );
    assert!(app.undo());
    assert_eq!(text(&app), "it ", "the next undo takes back the typed word");
}

#[test]
fn contractions_typed_on_a_keyboard_get_their_apostrophe() {
    let (mut app, page) = with_note("Autocorrect", "");
    type_in_body(&mut app, "it doesnt work, cant stop");
    let app = app.reopen().unwrap();
    assert_eq!(
        app.read_document(|doc| read::page_blocks(doc, page))[0].text,
        "it doesn't work, can't stop"
    );

    let (mut app, page) = with_note("Revert", "");
    type_in_body(&mut app, "it doesnt ");
    app.harness().key("Backspace");
    assert_eq!(
        app.read_document(|doc| read::page_blocks(doc, page))[0].text,
        "it doesnt",
        "Backspace right after the correction reverts it"
    );

    let (mut app, page) = with_note("Code", "");
    type_in_body(&mut app, "```");
    app.harness().key("Enter");
    app.harness().type_text("it doesnt work");
    let blocks = app.read_document(|doc| read::page_blocks(doc, page));
    assert_eq!(
        blocks.last().unwrap().text,
        "it doesnt work",
        "code is never corrected: {blocks:?}"
    );

    let (mut app, page) = with_note("Inline code", "");
    type_in_body(&mut app, "run `doesnt` now");
    assert_eq!(stored_markdown(&app, page), "run `doesnt` now\n");
}
