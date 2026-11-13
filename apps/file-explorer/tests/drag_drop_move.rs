//! Drag and drop: a row, or a multi-selection, carried into a folder or onto
//! the sidebar.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use crate::common::{Harness, row_center};

use vello::kurbo::Point;

fn folder_center(h: &Harness, name: &str) -> Point {
    let (_p, rect) =
        h.w.state()
            .debug_folder_targets()
            .into_iter()
            .find(|(p, _)| p.file_name().is_some_and(|n| n == name))
            .unwrap_or_else(|| panic!("folder {name} is not a published drop target"));
    rect.center()
}

fn drag_from_to(h: &mut Harness, source: Point, target: Point) {
    h.move_cursor(source);
    h.press();
    h.move_cursor(Point::new(source.x + 16.0, source.y));
    assert!(
        h.w.state().debug_pin_dragging(),
        "the threshold-crossing move started a drag"
    );
    h.move_cursor(target);
    h.release();
}

#[test]
fn drag_and_drop_moves_files_selections_and_up_but_not_into_itself() {
    dragging_a_file_onto_a_folder_moves_it();
    dragging_a_multi_selection_moves_every_item();
    dropping_on_the_up_affordance_moves_to_the_parent();
    dropping_a_folder_on_itself_is_a_noop();
}

fn dragging_a_file_onto_a_folder_moves_it() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir(d.path().join("dest")).unwrap(); // folder → row 0
    std::fs::write(d.path().join("movable.txt"), b"hello").unwrap(); // file → row 1

    let file_pt = row_center(d.path(), 1);
    let mut h = Harness::new(d.path());
    let dest_pt = folder_center(&h, "dest");

    drag_from_to(&mut h, file_pt, dest_pt);

    assert!(
        d.path().join("dest/movable.txt").exists(),
        "the file was moved into the folder"
    );
    assert!(
        !d.path().join("movable.txt").exists(),
        "the file no longer sits in the source dir"
    );
    assert_eq!(
        std::fs::read(d.path().join("dest/movable.txt")).unwrap(),
        b"hello",
        "the moved file keeps its contents (a real move, not a stub)",
    );
}

fn dragging_a_multi_selection_moves_every_item() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir(d.path().join("dest")).unwrap(); // row 0
    std::fs::write(d.path().join("a.txt"), b"a").unwrap(); // row 1
    std::fs::write(d.path().join("b.txt"), b"b").unwrap(); // row 2

    let a_pt = row_center(d.path(), 1);
    let mut h = Harness::new(d.path());
    h.w.state().debug_select(&[1, 2]);
    h.layout();
    let dest_pt = folder_center(&h, "dest");

    drag_from_to(&mut h, a_pt, dest_pt);

    assert!(d.path().join("dest/a.txt").exists(), "a.txt moved");
    assert!(
        d.path().join("dest/b.txt").exists(),
        "b.txt moved (the rest of the selection)"
    );
    assert!(
        !d.path().join("a.txt").exists() && !d.path().join("b.txt").exists(),
        "sources gone"
    );
}

fn dropping_on_the_up_affordance_moves_to_the_parent() {
    let d = tempfile::tempdir().unwrap();
    let sub = d.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join("up.txt"), b"x").unwrap(); // the only row (row 0)

    let file_pt = row_center(&sub, 0);
    let mut h = Harness::new(&sub);
    let up =
        h.w.state()
            .debug_up_button_rect()
            .expect("a subdirectory has an Up button");

    drag_from_to(&mut h, file_pt, up.center());

    assert!(
        d.path().join("up.txt").exists(),
        "the file moved up into the parent"
    );
    assert!(!sub.join("up.txt").exists(), "and left the subdirectory");
}

fn dropping_a_folder_on_itself_is_a_noop() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir(d.path().join("keep")).unwrap(); // row 0
    std::fs::write(d.path().join("keep/inner.txt"), b"x").unwrap();

    let folder_pt = row_center(d.path(), 0);
    let mut h = Harness::new(d.path());

    drag_from_to(&mut h, folder_pt, folder_pt);

    assert!(
        d.path().join("keep/inner.txt").exists(),
        "the folder is untouched"
    );
    assert!(
        !d.path().join("keep/keep").exists(),
        "the folder was not moved into itself"
    );
}
