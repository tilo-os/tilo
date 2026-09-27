//! Double-click-to-open must be reliable, including the case the review flagged
//! as "only sometimes works": a *still* double-click whose second press arrives
//! with no pointer movement after the host has rebuilt the view.
//!
//! The file explorer rebuilds its whole child tree every frame. The file view
//! hit-tests a press against its own `last_cursor`; a freshly rebuilt view that
//! is not seeded with the real cursor starts at `(0, 0)`, so a press with no
//! preceding `CursorMoved` misses the row and the folder never opens. These
//! tests drive the real widget through a rebuild between the two clicks and
//! prove the second click still opens the folder.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use crate::common::{Harness, row_center};

/// A temp dir containing a single folder (row 0) plus a file.
fn dir_with_folder() -> (tempfile::TempDir, std::path::PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let folder = d.path().join("Aardvark"); // folders lead the sort anyway
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(d.path().join("z-note.txt"), b"x").unwrap();
    (d, folder)
}

/// Single click, a still double-click across a rebuild, and a fast
/// double-click without one — each phase on its own fresh fixture and harness.
#[test]
fn clicks_select_and_double_clicks_open_the_folder_across_rebuilds() {
    single_click_selects_row_zero_but_does_not_open();
    double_click_across_a_rebuild_opens_the_folder();
    fast_double_click_without_a_rebuild_also_opens();
}

fn single_click_selects_row_zero_but_does_not_open() {
    let (dir, _folder) = dir_with_folder();
    let p = row_center(dir.path(), 0);
    let mut h = Harness::new(dir.path());
    let start = h.w.current_path();
    h.move_cursor(p);
    h.press();
    h.release();
    assert_eq!(
        h.w.selected_indices(),
        vec![0],
        "the row under the cursor is selected"
    );
    assert_eq!(h.w.current_path(), start, "a single click never navigates");
}

fn double_click_across_a_rebuild_opens_the_folder() {
    // The exact flaky path: the first click triggers a structural rebuild, and
    // the second press arrives with NO CursorMoved after it. A view not seeded
    // with the real cursor would hit-test the press against (0,0) and miss.
    let (dir, folder) = dir_with_folder();
    let p = row_center(dir.path(), 0);
    let mut h = Harness::new(dir.path());

    // First click (with the pointer move a real gesture has).
    h.move_cursor(p);
    h.press();
    h.release();
    assert_eq!(h.w.selected_indices(), vec![0]);

    // The host rebuilds the tree after the selection change.
    h.layout();

    // Second click — deliberately WITHOUT moving the pointer first.
    h.press();
    h.release();

    assert_eq!(
        h.w.current_path(),
        folder,
        "the still double-click opened the folder despite the intervening rebuild",
    );
}

fn fast_double_click_without_a_rebuild_also_opens() {
    // The normal path (no rebuild between the two clicks) must keep working.
    let (dir, folder) = dir_with_folder();
    let p = row_center(dir.path(), 0);
    let mut h = Harness::new(dir.path());
    h.move_cursor(p);
    h.press();
    h.release();
    h.press();
    h.release();
    assert_eq!(
        h.w.current_path(),
        folder,
        "a fast double-click opens the folder"
    );
}
