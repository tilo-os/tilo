//! End-to-end persistence: build a workspace with several pages,
//! save it to a tmpdir, reload, and assert nothing got lost.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use notes::Workspace;
use notes::storage::{load_workspace, save_workspace};
use tempfile::TempDir;

#[test]
fn workspace_persists_through_disk_and_restart() {
    eprintln!("phase three_page_workspace_roundtrips_through_disk");
    {
        let tmp = TempDir::new().unwrap();
        let mut ws = Workspace::new();
        let a = ws.create_page(None);
        let b = ws.create_page(None);
        let c = ws.create_page(Some(b));
        ws.rename(a, "Inbox");
        ws.rename(b, "Projects");
        ws.rename(c, "Project Alpha");
        ws.set_selected(Some(c));

        save_workspace(tmp.path(), &ws).unwrap();
        let loaded = load_workspace(tmp.path()).unwrap().expect("present");

        assert_eq!(loaded.pages().len(), 3);
        assert_eq!(loaded.get(a).unwrap().title, "Inbox");
        assert_eq!(loaded.get(b).unwrap().title, "Projects");
        assert_eq!(loaded.get(c).unwrap().title, "Project Alpha");
        assert_eq!(loaded.get(c).unwrap().parent, Some(b));
        assert_eq!(loaded.selected(), Some(c));

        // Sibling order is preserved: a comes before b at the root level.
        let roots = loaded.children_of(None);
        assert_eq!(roots[0].id, a);
        assert_eq!(roots[1].id, b);

        loaded.check_invariants().unwrap();
    }

    eprintln!("phase missing_workspace_file_loads_as_none");
    {
        let tmp = TempDir::new().unwrap();
        let res = load_workspace(tmp.path()).unwrap();
        assert!(
            res.is_none(),
            "fresh tmpdir should produce a None workspace"
        );
    }

    eprintln!("phase notes_app_spawn_restart_preserves_welcome_page");
    {
        use notes::NotesApp;
        let tmp = TempDir::new().unwrap();
        let mut first = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
        let id = first.workspace_pages_for_test()[0].id;
        first.flush_saves();
        drop(first);

        // Second launch should rehydrate the same Welcome page rather
        // than seeding a fresh one (different UUID).
        let second = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
        let pages = second.workspace_pages_for_test();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].id, id, "rehydrated page must keep its UUID");
        assert_eq!(pages[0].title, "Welcome");
    }
}
