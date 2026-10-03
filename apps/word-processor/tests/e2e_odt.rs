//! End-to-end: author a `.odt`, open it in the word processor, edit it, save,
//! reopen, and verify the edits persisted through the `.odt` codec + file I/O.
//! This exercises the whole stack — file bytes ↔ ODF codec ↔ canonical CRDT ↔
//! the editing model — the way the integrated OS app does.

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

use ui::Widget;
use word_processor::{WordProcessorWidget, WpFormat, open_document, save_document};

fn temp_path(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("wp-e2e-{}-{}", std::process::id(), name));
    p
}

/// Author a `.odt` on disk containing a single paragraph.
fn author_odt(path: &Path, text: &str) {
    let mut state = ui::State::new();
    let c = tilo_doc::model::init(&mut state.doc, &state.root);
    let mut ids = tilo_doc::IdGen::new(state.doc.get_actor().to_hex_string());
    {
        let mut dm = tilo_doc::DocMut {
            doc: &mut state.doc,
            c: &c,
            ids: &mut ids,
        };
        let p = dm.insert_block(None, "paragraph");
        dm.splice_text(&p, 0, 0, text);
    }
    save_document(&state, path, WpFormat::Odt, &tilo_content::NoContent).unwrap();
}

/// Open-edit-save-reload works for ODT and Markdown, and a table created in the editor and a custom named
/// style survive an ODT save and reload.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn odt_and_markdown_edits_survive_save_and_reload() {
    // Phase: open_edit_save_reload_odt.
    {
        let path = temp_path("doc.odt");
        author_odt(&path, "alpha");

        // Open it in the word processor.
        let (mut state, mut wp, fmt) =
            open_document(&path, tilo_content::ContentStore::memory()).unwrap();
        assert_eq!(fmt, WpFormat::Odt);
        wp.sync(&mut state.doc, &state.root);
        assert_eq!(
            wp.model().block_texts(&state.doc),
            vec!["alpha".to_string()]
        );

        // Edit: append " beta" to the paragraph, then a new paragraph "gamma".
        wp.model_mut().move_end(&state.doc);
        wp.model_mut().insert_str(&mut state.doc, " beta");
        wp.model_mut().enter(&mut state.doc);
        wp.model_mut().insert_str(&mut state.doc, "gamma");

        // Save back to the same .odt.
        save_document(&state, &path, fmt, &tilo_content::NoContent).unwrap();

        // Reopen and verify the edits round-tripped through the file.
        let (mut state2, mut wp2, _) =
            open_document(&path, tilo_content::ContentStore::memory()).unwrap();
        wp2.sync(&mut state2.doc, &state2.root);
        assert_eq!(
            wp2.model().block_texts(&state2.doc),
            vec!["alpha beta".to_string(), "gamma".to_string()],
        );

        std::fs::remove_file(&path).ok();
    }

    // Phase: open_edit_save_reload_markdown.
    {
        let path = temp_path("doc.md");
        std::fs::write(&path, b"# Title\n\nbody\n").unwrap();

        let (mut state, mut wp, fmt) =
            open_document(&path, tilo_content::ContentStore::memory()).unwrap();
        assert_eq!(fmt, WpFormat::Markdown);
        wp.sync(&mut state.doc, &state.root);
        // heading + paragraph
        assert_eq!(
            wp.model().block_texts(&state.doc),
            vec!["Title".to_string(), "body".to_string()]
        );

        // type into the (last) paragraph
        wp.model_mut().move_to_block(&state.doc, 1);
        wp.model_mut().move_end(&state.doc);
        wp.model_mut().insert_str(&mut state.doc, " text");
        save_document(&state, &path, fmt, &tilo_content::NoContent).unwrap();

        let (mut state2, mut wp2, _) =
            open_document(&path, tilo_content::ContentStore::memory()).unwrap();
        wp2.sync(&mut state2.doc, &state2.root);
        assert_eq!(
            wp2.model().block_texts(&state2.doc),
            vec!["Title".to_string(), "body text".to_string()]
        );

        std::fs::remove_file(&path).ok();
    }

    // Phase: table_created_in_editor_survives_odt_save_reload.
    {
        let path = temp_path("table.odt");
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "Title");
        wp.model_mut().insert_table(&mut state.doc, 2, 2); // caret lands in first cell
        wp.model_mut().insert_str(&mut state.doc, "X");

        save_document(&state, &path, WpFormat::Odt, &tilo_content::NoContent).unwrap();

        let (mut s2, mut wp2, _) =
            open_document(&path, tilo_content::ContentStore::memory()).unwrap();
        wp2.sync(&mut s2.doc, &s2.root);
        // Title paragraph + a 2x2 table whose first cell kept "X".
        assert_eq!(
            wp2.model().block_texts(&s2.doc),
            vec![
                "Title".to_string(),
                "X".to_string(),
                String::new(),
                String::new(),
                String::new()
            ],
            "table + cell text round-tripped through .odt",
        );
        std::fs::remove_file(&path).ok();
    }

    // Phase: custom_named_style_survives_odt_save_reload.
    {
        use word_processor::StyleProps;

        let path = temp_path("styled.odt");

        // Author: a paragraph carrying a user-defined named style.
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "Heading text");

        let mut sheet = wp.model().stylesheet(&state.doc);
        sheet.styles.insert(
            "callout".into(),
            StyleProps {
                name: "Callout".into(),
                font: "serif".into(),
                size: 22.0,
                bold: true,
                italic: true,
                color: [0xc0, 0x20, 0x20],
                align: "center".into(),
                space_before: 0.0,
                space_after: 10.0,
                indent: 0.0,
            },
        );
        wp.model_mut().set_stylesheet(&mut state.doc, &sheet);
        wp.model_mut().set_block_style(&mut state.doc, "callout");

        save_document(&state, &path, WpFormat::Odt, &tilo_content::NoContent).unwrap();

        // Reopen: the style is back in the stylesheet, still applied to the block.
        let (mut s2, mut wp2, _) =
            open_document(&path, tilo_content::ContentStore::memory()).unwrap();
        wp2.sync(&mut s2.doc, &s2.root);
        assert_eq!(
            wp2.model().block_texts(&s2.doc),
            vec!["Heading text".to_string()]
        );

        let sheet2 = wp2.model().stylesheet(&s2.doc);
        let cs = sheet2.get("callout");
        assert_eq!(cs.name, "Callout", "custom style present after reload");
        assert_eq!(cs.size, 22.0);
        assert!(cs.bold && cs.italic);
        assert_eq!(cs.color, [0xc0, 0x20, 0x20], "color round-tripped");
        assert_eq!(cs.align, "center");
        assert_eq!(cs.space_after, 10.0);
        // Built-ins must still resolve (the import only writes referenced named styles).
        assert_eq!(
            sheet2.get("body").name,
            "Body",
            "built-in body still available after import"
        );

        // The block still references the named style.
        let ed = word_processor::EditorDoc::bind(&mut s2.doc, &s2.root);
        let snap = ed.snapshot(&s2.doc);
        assert_eq!(
            snap.blocks[0].attrs.get("style"),
            Some(&tilo_doc::AttrVal::Str("callout".into())),
            "block keeps its named-style ref: {:?}",
            snap.blocks[0].attrs
        );
        assert!(
            snap.styles.contains_key("callout"),
            "styles container holds the custom style after reload"
        );

        std::fs::remove_file(&path).ok();
    }
}

/// Undo reverts a word-processor edit and two word processors collaborate and converge.
#[test]
fn the_editor_undoes_and_converges_across_word_processors() {
    // Phase: undo_reverts_word_processor_edit.
    {
        // The host App snapshots on commit_action and restores on undo; the word
        // processor only needs to edit the shared doc + signal commits.
        let mut s = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut s.doc, &s.root);
        s.commit_action(None, None); // baseline: one empty paragraph

        wp.model_mut().insert_str(&mut s.doc, "hello");
        s.commit_action(None, None);
        assert_eq!(wp.model().block_texts(&s.doc), vec!["hello".to_string()]);

        s.undo(None, None);
        assert_eq!(
            wp.model().block_texts(&s.doc),
            vec![String::new()],
            "undo reverts the edit"
        );
    }

    // Phase: two_word_processors_collaborate_and_converge.
    {
        // Peer A starts a shared document.
        let mut sa = ui::State::new();
        let mut wpa = WordProcessorWidget::new();
        wpa.sync(&mut sa.doc, &sa.root);
        wpa.model_mut().insert_str(&mut sa.doc, "shared");
        sa.commit_action(None, None);

        // Peer B forks A's state (distinct actor) and opens the same document.
        let bytes = sa.save();
        let mut sb = ui::State::load(&bytes).unwrap();
        sb.doc.set_actor(automerge::ActorId::random());
        let mut wpb = WordProcessorWidget::new();
        wpb.sync(&mut sb.doc, &sb.root);
        assert_eq!(wpb.model().block_texts(&sb.doc), vec!["shared".to_string()]);

        // Concurrent edits: each appends to the shared paragraph.
        wpa.model_mut().move_end(&sa.doc);
        wpa.model_mut().insert_str(&mut sa.doc, " A");
        wpb.model_mut().move_end(&sb.doc);
        wpb.model_mut().insert_str(&mut sb.doc, " B");

        // Exchange incremental changes (the live-collab path).
        let da = sa.save_incremental();
        let db = sb.save_incremental();
        sa.load_incremental(&db).unwrap();
        sb.load_incremental(&da).unwrap();
        wpa.sync(&mut sa.doc, &sa.root);
        wpb.sync(&mut sb.doc, &sb.root);

        // Both replicas converge to the same document, containing both edits.
        let ta = wpa.model().block_texts(&sa.doc);
        let tb = wpb.model().block_texts(&sb.doc);
        assert_eq!(ta, tb, "collaborating word processors converge");
        assert_eq!(ta.len(), 1);
        assert!(
            ta[0].contains("shared") && ta[0].contains('A') && ta[0].contains('B'),
            "both edits present: {ta:?}"
        );
    }
}
