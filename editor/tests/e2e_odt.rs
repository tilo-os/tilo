//! End-to-end: author a `.odt`, open it in the word processor, edit it, save,
//! reopen, and verify the edits persisted through the `.odt` codec + file I/O.
//! This exercises the whole stack — file bytes ↔ ODF codec ↔ canonical CRDT ↔
//! the editing model — the way the integrated OS app does.

use std::path::{Path, PathBuf};

use editor::{open_document, save_document, WpFormat};
use ui::Widget;

fn temp_path(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("wp-e2e-{}-{}", std::process::id(), name));
    p
}

/// Author a `.odt` on disk containing a single paragraph.
fn author_odt(path: &Path, text: &str) {
    let mut state = ui::State::new();
    let c = qdoc::model::init(&mut state.doc, &state.root);
    let mut ids = qdoc::IdGen::new(state.doc.get_actor().to_hex_string());
    {
        let mut dm = qdoc::DocMut { doc: &mut state.doc, c: &c, ids: &mut ids };
        let p = dm.insert_block(None, "paragraph");
        dm.splice_text(&p, 0, 0, text);
    }
    save_document(&state, path, WpFormat::Odt).unwrap();
}

#[test]
fn open_edit_save_reload_odt() {
    let path = temp_path("doc.odt");
    author_odt(&path, "alpha");

    // Open it in the word processor.
    let (mut state, mut wp, fmt) = open_document(&path).unwrap();
    assert_eq!(fmt, WpFormat::Odt);
    wp.sync(&mut state.doc, &state.root);
    assert_eq!(wp.model().block_texts(&state.doc), vec!["alpha".to_string()]);

    // Edit: append " beta" to the paragraph, then a new paragraph "gamma".
    wp.model_mut().move_end(&state.doc);
    wp.model_mut().insert_str(&mut state.doc, " beta");
    wp.model_mut().enter(&mut state.doc);
    wp.model_mut().insert_str(&mut state.doc, "gamma");

    // Save back to the same .odt.
    save_document(&state, &path, fmt).unwrap();

    // Reopen and verify the edits round-tripped through the file.
    let (mut state2, mut wp2, _) = open_document(&path).unwrap();
    wp2.sync(&mut state2.doc, &state2.root);
    assert_eq!(
        wp2.model().block_texts(&state2.doc),
        vec!["alpha beta".to_string(), "gamma".to_string()],
    );

    std::fs::remove_file(&path).ok();
}

#[test]
fn open_edit_save_reload_markdown() {
    let path = temp_path("doc.md");
    std::fs::write(&path, b"# Title\n\nbody\n").unwrap();

    let (mut state, mut wp, fmt) = open_document(&path).unwrap();
    assert_eq!(fmt, WpFormat::Markdown);
    wp.sync(&mut state.doc, &state.root);
    // heading + paragraph
    assert_eq!(wp.model().block_texts(&state.doc), vec!["Title".to_string(), "body".to_string()]);

    // type into the (last) paragraph
    wp.model_mut().move_to_block(&state.doc, 1);
    wp.model_mut().move_end(&state.doc);
    wp.model_mut().insert_str(&mut state.doc, " text");
    save_document(&state, &path, fmt).unwrap();

    let (mut state2, mut wp2, _) = open_document(&path).unwrap();
    wp2.sync(&mut state2.doc, &state2.root);
    assert_eq!(wp2.model().block_texts(&state2.doc), vec!["Title".to_string(), "body text".to_string()]);

    std::fs::remove_file(&path).ok();
}
