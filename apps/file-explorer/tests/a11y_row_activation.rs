#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

//! A row of Files answers an assistive technology's activation as a double-click opens
//! it.

use super::common::{Harness, row_center};
use ui::headless::Framed;

fn dir_with_folder() -> (tempfile::TempDir, std::path::PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let folder = d.path().join("Aardvark");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(d.path().join("z-note.txt"), b"x").unwrap();
    (d, folder)
}

#[test]
fn a_double_click_opens_the_folder_row() {
    let (dir, folder) = dir_with_folder();
    let p = row_center(dir.path(), 0);
    let mut h = Harness::new(dir.path());
    for _ in 0..2 {
        h.move_cursor(p);
        h.press();
        h.release();
        h.layout();
    }
    assert_eq!(
        h.w.state().current_path(),
        folder,
        "a double-click on the folder row opens it"
    );
}

#[test]
fn an_assistive_technology_opens_the_folder_row() {
    let (dir, folder) = dir_with_folder();
    let mut h = Harness::new(dir.path());
    let node = Framed::harness_mut(&mut h.w)
        .a11y_node_named("Aardvark")
        .expect("the row is described by name");
    Framed::harness_mut(&mut h.w)
        .perform_a11y_action(node, &ui::a11y::Action::Activate)
        .expect("the row answers an activation");
    h.layout();
    assert_eq!(
        h.w.state().current_path(),
        folder,
        "the activated folder row opens"
    );
}

#[test]
fn an_assistive_technology_opens_the_row_menu() {
    let (dir, _folder) = dir_with_folder();
    let mut h = Harness::new(dir.path());
    assert!(
        Framed::harness_mut(&mut h.w)
            .a11y_node_named("Rename")
            .is_none(),
        "no menu is open before the row is asked"
    );
    let node = Framed::harness_mut(&mut h.w)
        .a11y_node_named("Aardvark")
        .expect("the row is described by name");
    Framed::harness_mut(&mut h.w)
        .perform_a11y_action(node, &ui::a11y::Action::ShowContextMenu)
        .expect("the row answers a context-menu request");
    h.layout();
    assert!(
        Framed::harness_mut(&mut h.w)
            .a11y_node_named("Rename")
            .is_some(),
        "the menu the row asked for opens, on that row"
    );
    assert_eq!(
        h.w.state().current_path(),
        dir.path(),
        "asking a row for its menu opens the menu, not the folder"
    );
}
