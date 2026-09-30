//! A failed page-body save must surface in the status bar as
//! `SaveStatus::Failed` — the worst-of(workspace, page) merge that
//! `refresh_status` computes. Drives the real background `PageSaver`
//! through `NotesApp` and asserts the state the status bar renders.
//!
//! The failure is forced deterministically (independent of the test
//! user's privileges): the page's on-disk path is replaced with a
//! directory, so `std::fs::write` can never succeed there.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use ui::time::Duration;

use notes::sidebar::OutEvent;
use notes::storage::SaveStatus;
use notes::{NotesApp, storage};
use tempfile::TempDir;
use ui::widget::Widget;

/// Bounds a wait that ends as soon as its condition holds; only a hang reaches it.
const HANG_BOUND: Duration = Duration::from_secs(30);

#[test]
fn status_bar_surfaces_page_save_failure() {
    let tmp = TempDir::new().unwrap();
    let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();

    // Fresh page → CreateChild writes an empty body file for it.
    app.apply_event_for_test(OutEvent::CreateChild { parent: None });
    let id = app.selected_for_test().unwrap();

    // Replace the page's file with a directory so every save fails,
    // regardless of the running user's permissions.
    let path = storage::page_file_path(tmp.path(), id);
    let _ = std::fs::remove_file(&path);
    std::fs::create_dir(&path).unwrap();

    // Dirty the editor for this page and flush the savers so the
    // background writer attempts (and fails) the body write.
    app.main_panel_mut()
        .load_page(id, "Doomed", "content that cannot be saved");
    app.main_panel_mut().editor_mut().force_dirty_for_test();
    app.tick_for_test(); // drain_editor_edits → page_saver.request(...)
    // Clear the editor's lingering structure-dirty flag (the App does
    // this every frame via take_structure_dirty) so the poll below can't
    // re-queue the save and starve the saver's debounce.
    let _ = app.take_structure_dirty();
    app.flush_saves(); // flush → worker writes → EISDIR → Failed

    // Poll the status bar (refresh_status re-reads the saver status)
    // until it reflects the failure. Status-only refresh, so no re-queue.
    // Bounded wait on the writer thread — a directory write fails fast.
    let mut surfaced = false;
    let deadline = ui::time::now() + HANG_BOUND;
    while ui::time::now() < deadline {
        app.refresh_status_for_test();
        if app.status_bar_for_test().state().save_status == SaveStatus::Failed {
            surfaced = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    assert!(
        surfaced,
        "a failed page-body save must surface as SaveStatus::Failed in the status bar"
    );
}
