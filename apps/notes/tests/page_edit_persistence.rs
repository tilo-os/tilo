//! End-to-end: drive a `NotesApp` through a synthetic edit on the
//! currently-selected page, request a save, drop the app, then
//! reload and verify the on-disk body matches.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::Path;

use notes::NotesApp;
use notes::storage;
use tempfile::TempDir;
use ui::widget::{BlockEditor, BlockKind};

/// Convenience: read a page's body off disk and return it. `Err` if
/// the file is missing — the test must have produced one.
fn read_body(root: &Path, id: uuid::Uuid) -> std::io::Result<String> {
    storage::read_page_body(root, id)
}

#[test]
fn block_editor_edits_splits_and_promotions_persist_to_disk() {
    eprintln!("phase editing_a_page_through_the_block_editor_persists_to_disk");
    {
        let tmp = TempDir::new().unwrap();
        let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).expect("spawn");

        // Seed page: the Welcome page lives there from the first-run
        // hook. Grab its id off the workspace.
        let welcome_id = app.workspace_pages_for_test()[0].id;

        // Drive an edit via the public editor mutator. We bypass the
        // winit event path so the test stays deterministic — the
        // BlockEditor's mutations are the same regardless of whether
        // they came from a key press or a programmatic split.
        {
            let panel = app.main_panel_mut();
            panel.editor_mut().blocks_mut()[0].line.set_focus(true);
            panel.editor_mut().blocks_mut()[0]
                .line
                .set_text("# Hello\n");
            // Mark the editor dirty manually — set_text is programmatic
            // (it doesn't flag dirty) but the test wants to assert the
            // save pipeline picks up changes the user could otherwise
            // make. Achieve the same by promoting an empty paragraph.
            panel.editor_mut().force_dirty_for_test();
        }

        // Tick the app: drains the editor's dirty flag and queues a
        // save. The PageSaver's worker is debounced, so we force-flush.
        app.tick_for_test();
        app.flush_saves();

        drop(app);

        // Verify disk.
        let body = read_body(tmp.path(), welcome_id).expect("body file exists");
        // After our edit the body should be "# Hello\n" turned into
        // markdown by the editor's serializer.
        assert!(body.contains("# Hello"), "got body: {body:?}");
    }

    eprintln!("phase split_and_save_round_trip_through_disk");
    {
        let tmp = TempDir::new().unwrap();
        let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).expect("spawn");
        let welcome_id = app.workspace_pages_for_test()[0].id;

        // Reset the welcome page so its demo blocks do not mix with the fixture.
        {
            let panel = app.main_panel_mut();
            panel.load_page(welcome_id, "Welcome", "");
        }

        // Seed: type "First", press Enter, type "Second" — via the
        // editor's split helper since we don't dispatch winit events here.
        {
            let panel = app.main_panel_mut();
            let editor = panel.editor_mut();
            editor.blocks_mut()[0].line.set_text("First");
            editor.blocks_mut()[0].line.set_focus(true);
            editor.blocks_mut()[0].line.set_cursor_pos("First".len());
            editor.split_at_cursor(0, "First".len());
            editor.blocks_mut()[1].line.set_text("Second");
            editor.force_dirty_for_test();
        }

        app.tick_for_test();
        app.flush_saves();
        drop(app);

        let body = read_body(tmp.path(), welcome_id).expect("body file exists");
        // Two paragraphs, separated by blank line, trailing newline.
        assert_eq!(body, "First\n\nSecond\n");

        // Round-trip back through the editor.
        let editor = BlockEditor::from_markdown(&body);
        assert_eq!(editor.blocks().len(), 2);
        assert_eq!(editor.blocks()[0].text(), "First");
        assert_eq!(editor.blocks()[1].text(), "Second");
        assert!(matches!(editor.blocks()[0].kind, BlockKind::Paragraph));
    }

    eprintln!("phase heading_promotion_survives_save_reload");
    {
        let tmp = TempDir::new().unwrap();
        let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).expect("spawn");
        let welcome_id = app.workspace_pages_for_test()[0].id;

        // Heading promotion only acts on an empty paragraph, so discard the
        // starter page's demo blocks.
        {
            let panel = app.main_panel_mut();
            panel.load_page(welcome_id, "Welcome", "");
        }

        {
            let panel = app.main_panel_mut();
            let editor = panel.editor_mut();
            editor.blocks_mut()[0].line.set_text("## Heading two");
            editor.blocks_mut()[0].line.set_focus(true);
            // Promote: try_promote_to_heading parses the prefix and
            // strips it, leaving an H2 block with text "Heading two".
            let promoted = editor.try_promote_to_heading(0);
            assert!(promoted);
            assert!(matches!(editor.blocks()[0].kind, BlockKind::Heading(2)));
            editor.force_dirty_for_test();
        }

        app.tick_for_test();
        app.flush_saves();
        drop(app);

        let body = read_body(tmp.path(), welcome_id).expect("body file exists");
        assert_eq!(body, "## Heading two\n");

        // Reload the app from disk and assert the heading parsed back.
        let mut app2 = NotesApp::spawn_with_starter_page(tmp.path().into()).expect("reload");
        let panel = app2.main_panel_mut();
        assert_eq!(panel.editor().blocks().len(), 1);
        assert!(matches!(
            panel.editor().blocks()[0].kind,
            BlockKind::Heading(2)
        ));
        assert_eq!(panel.editor().blocks()[0].text(), "Heading two");
    }
}
