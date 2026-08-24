//! A document that has just been opened is ready to be written in.
//!
//! Opening one and typing is the most ordinary thing anybody does with a word
//! processor, and it involves no click — so nothing here clicks. Every
//! keystroke goes in through the same `handle_event` dispatch winit delivers
//! to, and the assertions are on the document that came back out.
//!
//! The caret being *drawn* where these tests put it, and the style inspector
//! still being able to take the keyboard off the page, are proved next to the
//! geometry that decides them, in `widget.rs`.

use std::path::PathBuf;

use qdoc::model::Region;
use ui::headless::Driver;
use ui::widget::Widget;
use word_processor::{open_document, EditorDoc};

/// A file on disk to open, so the document arrives the way a person's does:
/// through `open_document`, decoded by the format's codec.
fn document_file(name: &str, body: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("word-processor-opens-ready-to-type");
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    let path = dir.join(name);
    std::fs::write(&path, body).expect("the document is written");
    path
}

/// The body text, block by block, read back out of the live document.
fn body_text(state: &mut ui::State) -> Vec<String> {
    let ed = EditorDoc::bind(&mut state.doc, &state.root);
    ed.blocks(&state.doc)
        .iter()
        .map(|id| ed.text(&state.doc, id).unwrap_or_default())
        .collect()
}

/// Type into a document that already has words in it, having touched nothing
/// first. The text belongs at the start of what is there — inserted before it,
/// not on top of it.
#[test]
fn typing_into_a_freshly_opened_document_lands_at_the_start_of_the_body() {
    let path = document_file("with-content.md", "Doors at seven.\n\nSecond floor.\n");
    let (state, wp, _fmt) = open_document(&path).expect("the document opens");
    let mut driver = Driver::new(state, Box::new(wp), (1000, 700));

    driver.type_text("Yes, ");

    let text = body_text(driver.state_mut());
    assert_eq!(
        text,
        vec!["Yes, Doors at seven.".to_string(), "Second floor.".to_string()],
        "the words went in at the start of the body, and nothing already there \
         was replaced: {text:?}",
    );
}

/// An empty document takes the first keystroke too. Its "Start writing…" prompt
/// is drawn onto the page rather than stored in it, so it cannot be typed over
/// as literal text — the document holds exactly what was typed.
#[test]
fn an_empty_document_takes_the_first_keystroke_and_keeps_no_placeholder_text() {
    let path = document_file("empty.md", "");
    let (state, wp, _fmt) = open_document(&path).expect("the empty document opens");
    let mut driver = Driver::new(state, Box::new(wp), (1000, 700));

    driver.type_text("Hello");

    let text = body_text(driver.state_mut());
    assert_eq!(
        text,
        vec!["Hello".to_string()],
        "the document holds the five characters that were typed and nothing \
         else: {text:?}",
    );
}

/// A caret put into a page header stays there.
///
/// `bind` runs at the top of every frame, and it used to check the caret
/// against the *body's* blocks — which never contain a running head's, since
/// those live in their own container. So a caret placed in the header band was
/// dragged back into the body one frame later, and the band could not be typed
/// into at all.
#[test]
fn a_caret_put_in_the_page_header_is_not_dragged_back_into_the_body() {
    let path = document_file("running-head.md", "Body line.\n");
    let (mut state, mut wp, _fmt) = open_document(&path).expect("the document opens");
    wp.sync(&mut state.doc, &state.root);

    let header_block = wp.model_mut().focus_region(&mut state.doc, Region::Header);
    assert!(!header_block.is_empty(), "the header band has a block to type into");

    // The next frame, before the person has done anything else.
    wp.sync(&mut state.doc, &state.root);

    assert_eq!(
        wp.model().caret().block,
        header_block,
        "the caret is still in the header band it was put in",
    );
    assert_eq!(
        wp.model().region(),
        Region::Header,
        "and the edit context is still the header",
    );
}
