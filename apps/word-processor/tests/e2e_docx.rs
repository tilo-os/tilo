//! End-to-end: author a rich document (heading, bold run, bullet list, and a
//! 2x2 table) in the word processor, save it as `.docx`, reopen, and verify the
//! structure (block kinds, text, inline marks) round-tripped through the `qdoc`
//! `DocxCodec` + the editor's file I/O — exactly as the integrated app does.
//! Mirrors `e2e_odt.rs` for the Microsoft Word format.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::{Path, PathBuf};

use tilo_doc::AttrVal;
use ui::Widget;
use word_processor::{WordProcessorWidget, WpFormat, open_document, save_document};

fn temp_path(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("wp-docx-e2e-{}-{}", std::process::id(), name));
    p
}

/// `from_path` maps the docx extension, and a rich document round-trips through docx.
#[test]
fn docx_files_are_recognised_and_rich_documents_round_trip() {
    // Phase: from_path_maps_docx_extension.
    {
        assert_eq!(
            WpFormat::from_path(Path::new("/tmp/report.docx")),
            WpFormat::Docx
        );
        assert_eq!(
            WpFormat::from_path(Path::new("/tmp/REPORT.DOCX")),
            WpFormat::Docx,
            "case-insensitive"
        );
    }

    // Phase: rich_document_round_trips_through_docx.
    {
        let path = temp_path("Bericht – 日本語.docx");

        // --- Author the document through the editor's real editing model. ---
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);

        // 1. A heading.
        wp.model_mut().insert_str(&mut state.doc, "Heading");
        wp.model_mut()
            .set_paragraph_style(&mut state.doc, "heading", Some(1), None);

        // 2. A paragraph with a bold run (select the whole text, toggle `strong`).
        wp.model_mut().enter(&mut state.doc);
        wp.model_mut().insert_str(&mut state.doc, "bold");
        wp.model_mut().move_home();
        wp.model_mut().set_anchor();
        wp.model_mut().move_end(&state.doc);
        wp.model_mut().toggle_mark(&mut state.doc, "strong");
        wp.model_mut().clear_anchor();

        // 3. A bullet (unordered) list item.
        wp.model_mut().enter(&mut state.doc);
        wp.model_mut().insert_str(&mut state.doc, "Bullet");
        wp.model_mut()
            .set_paragraph_style(&mut state.doc, "list_item", None, Some(false));

        // 4. A 2x2 table; the caret lands in the first cell, type into it.
        wp.model_mut().insert_table(&mut state.doc, 2, 2);
        wp.model_mut().insert_str(&mut state.doc, "X");

        // --- Save as `.docx` via the editor's export path. ---
        save_document(&state, &path, WpFormat::Docx, &tilo_content::NoContent).unwrap();

        // --- Reopen into a fresh ui::State and inspect the canonical structure. ---
        let (state2, _wp2, fmt) =
            open_document(&path, tilo_content::ContentStore::memory()).unwrap();
        assert_eq!(fmt, WpFormat::Docx);
        let c = tilo_doc::model::open(&state2.doc, &state2.root).unwrap();
        let snap = tilo_doc::model::materialize(&state2.doc, &c);

        // Heading survived with its level.
        let heading = snap
            .blocks
            .iter()
            .find(|b| b.kind == "heading")
            .expect("heading survived");
        assert_eq!(heading.text, "Heading");
        assert_eq!(
            heading.attrs.get("level"),
            Some(&AttrVal::Int(1)),
            "heading level 1"
        );

        // Bold run: a paragraph whose `bold` text carries a `strong` mark over it.
        let para = snap
            .blocks
            .iter()
            .find(|b| b.kind == "paragraph" && b.text == "bold")
            .expect("bold paragraph survived");
        let strong = para
            .marks
            .iter()
            .find(|m| m.name == "strong")
            .expect("strong mark survived");
        assert_eq!(
            (strong.start, strong.end),
            (0, 4),
            "strong covers the whole run"
        );

        // Bullet list item, marked unordered.
        let item = snap
            .blocks
            .iter()
            .find(|b| b.kind == "list_item")
            .expect("list item survived");
        assert_eq!(item.text, "Bullet");
        assert_eq!(
            item.attrs.get("ordered"),
            Some(&AttrVal::Bool(false)),
            "bullet (unordered)"
        );

        // 2x2 table: four `cell` blocks, the first holding "X".
        let cells: Vec<_> = snap.blocks.iter().filter(|b| b.kind == "cell").collect();
        assert_eq!(cells.len(), 4, "2x2 = four cells: {snap:?}");
        assert_eq!(cells[0].text, "X", "first cell kept its text");
        assert!(
            cells[1..].iter().all(|cell| cell.text.is_empty()),
            "remaining cells empty"
        );

        std::fs::remove_file(&path).ok();
    }
}
