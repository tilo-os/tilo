//! Double-click to open, across the rebuild the first click causes.
#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use crate::common::{Harness, row_center};

fn dir_with_folder() -> (tempfile::TempDir, std::path::PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let folder = d.path().join("Aardvark"); // folders lead the sort anyway
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(d.path().join("z-note.txt"), b"x").unwrap();
    (d, folder)
}

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
    let start = h.w.state().current_path();
    h.move_cursor(p);
    h.press();
    h.release();
    assert_eq!(
        h.w.state().selected_indices(),
        vec![0],
        "the row under the cursor is selected"
    );
    assert_eq!(
        h.w.state().current_path(),
        start,
        "a single click never navigates"
    );
}

fn double_click_across_a_rebuild_opens_the_folder() {
    let (dir, folder) = dir_with_folder();
    let p = row_center(dir.path(), 0);
    let mut h = Harness::new(dir.path());

    h.move_cursor(p);
    h.press();
    h.release();
    assert_eq!(h.w.state().selected_indices(), vec![0]);

    h.layout();

    h.press();
    h.release();

    assert_eq!(
        h.w.state().current_path(),
        folder,
        "the still double-click opened the folder despite the intervening rebuild",
    );
}

fn fast_double_click_without_a_rebuild_also_opens() {
    let (dir, folder) = dir_with_folder();
    let p = row_center(dir.path(), 0);
    let mut h = Harness::new(dir.path());
    h.move_cursor(p);
    h.press();
    h.release();
    h.press();
    h.release();
    assert_eq!(
        h.w.state().current_path(),
        folder,
        "a fast double-click opens the folder"
    );
}
