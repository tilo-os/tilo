//! Double-click to open, across the rebuild the first click causes.
#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::Path;

use crate::common::{Harness, row_center};
use vello::kurbo::Point;

fn dir_with_folder() -> (tempfile::TempDir, std::path::PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let folder = d.path().join("Aardvark"); // folders lead the sort anyway
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(d.path().join("z-note.txt"), b"x").unwrap();
    (d, folder)
}

fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// A scenario: the directory, the folder in it, and the centre of the folder's row.
type Scenario = fn(&Path, &Path, Point);

/// The three scenarios read one directory and write nothing to it, and each opens a
/// window of its own on it: the centre of its first row is found once, and the
/// directory is checked to be as it was after each of them.
#[test]
fn clicks_select_and_double_clicks_open_the_folder_across_rebuilds() {
    let (dir, folder) = dir_with_folder();
    let p = row_center(dir.path(), 0);
    let scenarios: [(&str, Scenario); 3] = [
        (
            "a single click",
            single_click_selects_row_zero_but_does_not_open,
        ),
        (
            "a double-click across a rebuild",
            double_click_across_a_rebuild_opens_the_folder,
        ),
        (
            "a fast double-click",
            fast_double_click_without_a_rebuild_also_opens,
        ),
    ];
    for (name, scenario) in scenarios {
        scenario(dir.path(), &folder, p);
        assert_eq!(
            names_in(dir.path()),
            ["Aardvark", "z-note.txt"],
            "{name} left the directory as it was"
        );
        assert_eq!(
            names_in(&folder),
            [""; 0],
            "{name} left the folder as it was"
        );
    }
}

fn single_click_selects_row_zero_but_does_not_open(dir: &Path, _folder: &Path, p: Point) {
    let mut h = Harness::new(dir);
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

fn double_click_across_a_rebuild_opens_the_folder(dir: &Path, folder: &Path, p: Point) {
    let mut h = Harness::new(dir);

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

fn fast_double_click_without_a_rebuild_also_opens(dir: &Path, folder: &Path, p: Point) {
    let mut h = Harness::new(dir);
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
