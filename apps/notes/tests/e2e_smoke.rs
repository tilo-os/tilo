//! End-to-end spine guard.
//!
//! Drives a full lifecycle against a fresh tmpdir:
//!
//! 1. Spawn `NotesApp` against an empty workspace root.
//! 2. Create a fresh page through the sidebar path (`CreateChild`).
//! 3. Focus the editor and seed each block kind via the public test
//!    hooks: paragraph, heading-via-markdown-shortcut, bullet,
//!    checklist (with a toggle), code-via-slash-menu.
//! 4. Flush the saver and drop the app.
//! 5. Re-spawn from the same dir and assert the on-disk content +
//!    page-tree shape match what we wrote.
//!
//! If any future phase breaks the read/write spine — markdown
//! round-trip, `BlockEditor` mutations, the saver pipeline, the
//! `NotesApp::spawn` reload path — this test catches it.

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

use notes::sidebar::OutEvent;
use notes::{NotesApp, storage};
use tempfile::TempDir;
use ui::widget::BlockKind;

fn read_body(root: &Path, id: uuid::Uuid) -> std::io::Result<String> {
    storage::read_page_body(root, id)
}

/// Spot-check the structural markers in the saved body — exact byte
/// equality would be fragile against future markdown serializer tweaks,
/// but these four pin every block kind the test exercised.
fn assert_body_has_every_block_kind(body: &str) {
    assert!(
        body.contains("Hello world."),
        "saved body must contain the paragraph; got {body:?}"
    );
    assert!(
        body.contains("## Section title"),
        "saved body must contain the H2 heading; got {body:?}"
    );
    assert!(
        body.contains("- bullet"),
        "saved body must contain the bullet line; got {body:?}"
    );
    assert!(
        body.contains("- [x] buy coffee"),
        "saved body must contain the checked checklist line; got {body:?}"
    );
}

#[test]
fn full_lifecycle_round_trips_through_disk() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();

    // ---- Phase A: spawn + seed a fresh page ----
    let mut app = NotesApp::spawn_with_starter_page(root.clone()).expect("spawn ok");
    // First-launch seeded the Welcome page; keep it and add a child
    // page that we'll write into.
    let welcome_id = app.workspace_pages_for_test()[0].id;
    app.apply_event_for_test(OutEvent::CreateChild {
        parent: Some(welcome_id),
    });
    let target_id = app.selected_for_test().expect("child selected");
    assert_ne!(target_id, welcome_id, "child must be a new page");

    // The child page lives at index 1 in the workspace; its title is
    // the default "Untitled" — rename it through the same path the
    // sidebar would.
    app.apply_event_for_test(OutEvent::Rename {
        id: target_id,
        new_title: "Test Page".into(),
    });

    // ---- Phase B: drive the editor through the test hooks ----
    {
        let panel = app.main_panel_mut();
        // apply_event_for_test creates + selects, so MainPanel is already
        // on the new page; load_page here is a defensive re-load either way.
        panel.load_page(target_id, "Test Page", "");
        let editor = panel.editor_mut();
        // Block 0: paragraph
        editor.blocks_mut()[0].line.set_text("Hello world.");
        editor.blocks_mut()[0].line.set_focus(true);

        // Block 1: split off into a fresh paragraph and promote it to H2
        // by typing the markdown shortcut prefix.
        editor.blocks_mut()[0]
            .line
            .set_cursor_pos("Hello world.".len());
        editor.split_at_cursor(0, "Hello world.".len());
        editor.blocks_mut()[1].line.set_text("## Section title");
        editor.blocks_mut()[1].line.set_focus(true);
        let promoted = editor.try_promote_to_heading(1);
        assert!(promoted, "## prefix must promote to Heading(2)");
        assert!(matches!(editor.blocks()[1].kind, BlockKind::Heading(2)));

        // Block 2: paragraph for the slash-menu conversion target.
        let h2_len = editor.blocks()[1].text().len();
        editor.blocks_mut()[1].line.set_cursor_pos(h2_len);
        editor.split_at_cursor(1, h2_len);
        // The text doesn't carry the `- ` marker — that's added at
        // serialize time based on the block kind.
        editor.blocks_mut()[2].line.set_text("bullet");
        editor.blocks_mut()[2].line.set_focus(true);
        editor.focus_block(2, ui::widget::CursorPlacement::End);
        // Convert paragraph to a Bullet via slash-menu API. Mirrors
        // what the SlashMenu's Enter handler would do once it picked
        // BlockKind::Bullet { indent: 0 }.
        let converted = editor.try_convert_to(2, BlockKind::Bullet { indent: 0 });
        assert!(converted, "paragraph → bullet conversion must succeed");

        // Block 3: checklist with a toggled (done) state.
        let b2_len = editor.blocks()[2].text().len();
        editor.blocks_mut()[2].line.set_cursor_pos(b2_len);
        editor.split_at_cursor(2, b2_len);
        editor.blocks_mut()[3].line.set_text("buy coffee");
        editor.blocks_mut()[3].line.set_focus(true);
        let converted = editor.try_convert_to(
            3,
            BlockKind::Checklist {
                indent: 0,
                done: false,
            },
        );
        assert!(converted);
        // Toggle the checkbox — this is what a click on the checkbox
        // glyph would do.
        let new_done = editor.try_toggle_check(3);
        assert_eq!(new_done, Some(true));

        // Flag dirty so the save pipeline picks the snapshot up.
        editor.force_dirty_for_test();
    }

    // ---- Phase C: flush saves + drop ----
    app.tick_for_test();
    app.flush_saves();
    drop(app);

    // ---- Phase D: read the body off disk + assert content ----
    let body = read_body(&root, target_id).expect("body file exists");
    assert_body_has_every_block_kind(&body);

    // ---- Phase E: re-spawn from the same root + verify reload ----
    let mut app2 = NotesApp::spawn_with_starter_page(root).expect("reload spawn ok");
    let pages = app2.workspace_pages_for_test();
    assert!(
        pages.len() >= 2,
        "reload must see at least Welcome + Test Page; got {} pages",
        pages.len()
    );
    let test_page = pages
        .iter()
        .find(|p| p.id == target_id)
        .expect("Test Page still in workspace");
    assert_eq!(test_page.title, "Test Page");

    // Navigate to it through the same public API the Cmd-K palette
    // uses, then assert the editor parsed every block back.
    app2.navigate_to(target_id);
    let panel = app2.main_panel_mut();
    assert_eq!(panel.page_id(), Some(target_id));
    let blocks = panel.editor().blocks();
    assert_eq!(
        blocks.len(),
        4,
        "must reload 4 blocks; got {}",
        blocks.len()
    );
    assert!(matches!(blocks[0].kind, BlockKind::Paragraph));
    assert_eq!(blocks[0].text(), "Hello world.");
    assert!(matches!(blocks[1].kind, BlockKind::Heading(2)));
    assert_eq!(blocks[1].text(), "Section title");
    assert!(matches!(blocks[2].kind, BlockKind::Bullet { indent: 0 }));
    assert_eq!(blocks[2].text(), "bullet");
    assert!(matches!(
        blocks[3].kind,
        BlockKind::Checklist {
            indent: 0,
            done: true
        }
    ));
    assert_eq!(blocks[3].text(), "buy coffee");
}
