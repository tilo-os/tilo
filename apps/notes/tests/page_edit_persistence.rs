//! Typing in a note edits the notebook record: paragraphs and headings the
//! editor makes are there when the notebook opens again.

use crate::framework_support::with_note;
use notes::doc::NotesDoc;
use ui::widget::BlockKind;

/// Click into the open note's body and type `keys`, one key per entry.
fn type_in_body(app: &mut ui::framework::testing::TestApp<notes::NotesShell>, keys: &[&str]) {
    let editor = *app.harness().targets().get("block_editor").unwrap();
    app.harness().click_at(editor.x0 + 20.0, editor.y0 + 10.0);
    for key in keys {
        if key.chars().count() == 1 || *key == "Enter" {
            app.harness().key(key);
        } else {
            app.harness().type_text(key);
        }
    }
}

#[test]
fn dictation_ime_replaces_a_note_selection_with_one_undoable_final() {
    use winit::event::{Ime, WindowEvent};
    let (mut app, page) = with_note("Dictation fixture", "replace me");
    type_in_body(&mut app, &[]);
    app.harness().key("Home");
    app.harness().set_modifiers(&["shift".into()]);
    app.harness().key("End");
    app.harness().set_modifiers(&[]);
    let before = app.read_document(|doc| NotesDoc::page_blocks(doc, page));
    app.harness().send(&WindowEvent::Ime(Ime::Preedit(
        "temporary draft".into(),
        None,
    )));
    assert_eq!(
        app.read_document(|doc| NotesDoc::page_blocks(doc, page)),
        before
    );
    app.harness()
        .send(&WindowEvent::Ime(Ime::Commit("日本語 café العربية".into())));
    assert_eq!(
        app.read_document(|doc| NotesDoc::page_blocks(doc, page))[0].text,
        "日本語 café العربية"
    );
    assert!(app.undo(), "the final must use system history");
    assert_eq!(
        app.read_document(|doc| NotesDoc::page_blocks(doc, page)),
        before
    );
    assert!(app.redo());
    assert_eq!(
        app.read_document(|doc| NotesDoc::page_blocks(doc, page))[0].text,
        "日本語 café العربية"
    );
    let reopened = app.reopen().unwrap();
    assert_eq!(
        reopened.read_document(|doc| NotesDoc::page_blocks(doc, page))[0].text,
        "日本語 café العربية"
    );
}

#[test]
fn typed_paragraphs_and_headings_survive_a_reopen() {
    eprintln!("phase split_paragraphs_round_trip_through_the_record");
    {
        let (mut app, page) = with_note("First note", "");
        type_in_body(&mut app, &["First", "Enter", "Second"]);
        let app = app.reopen().unwrap();
        let blocks = app.read_document(|doc| NotesDoc::page_blocks(doc, page));
        let texts = blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>();
        assert_eq!(texts, ["First", "Second"]);
        assert!(
            blocks
                .iter()
                .all(|block| matches!(block.kind, BlockKind::Paragraph))
        );
    }

    eprintln!("phase heading_typed_with_its_markdown_prefix_survives_a_reopen");
    {
        let (mut app, page) = with_note("Headings", "");
        type_in_body(&mut app, &["## Heading two", "Enter", "Body"]);
        let app = app.reopen().unwrap();
        let blocks = app.read_document(|doc| NotesDoc::page_blocks(doc, page));
        assert!(
            matches!(blocks[0].kind, BlockKind::Heading(2)),
            "{blocks:?}"
        );
        assert_eq!(blocks[0].text, "Heading two");
        assert_eq!(blocks[1].text, "Body");
    }
}
