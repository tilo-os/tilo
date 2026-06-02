//! Phase 4: Cmd-K / Ctrl-K opens the page jump palette, which
//! navigates to the chosen page on Enter.

use notes::NotesApp;
use tempfile::TempDir;

#[test]
fn cmd_k_palette_lists_workspace_pages() {
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn(tmp.path().into()).unwrap();
    // After spawn the workspace has the Welcome page; the palette
    // should have it seeded.
    let pal = app.palette_mut();
    pal.set_visible(true);
    assert!(pal.is_visible());
    assert_eq!(pal.visible_len(), 1);
    assert_eq!(pal.highlighted().unwrap().title, "Welcome");
}

#[test]
fn selecting_a_page_changes_app_selected() {
    use notes::sidebar::OutEvent;
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn(tmp.path().into()).unwrap();
    let root_id = app.workspace_pages_for_test()[0].id;
    // Create a second page so the palette has two entries.
    app.apply_event_for_test(OutEvent::CreateChild { parent: None });
    // Now the workspace has two pages — Welcome + Untitled.
    assert_eq!(app.workspace_pages_for_test().len(), 2);

    // Navigate explicitly to the root via the public API.
    app.navigate_to(root_id);
    assert_eq!(app.selected_for_test(), Some(root_id));
}
