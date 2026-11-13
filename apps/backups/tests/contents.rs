//! The restore sheet's contents as a tree: the snapshot's folders and files, indented under
//! their folder, folded away and opened again.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    reason = "in a test a panic is the assertion"
)]

use backups::BackupsShell;
use ui::accesskit::Role;
use ui::framework::testing::TestApp;

/// The contents list's rows as a reader hears them: each row's name and the depth it sits
/// at.
fn rows(app: &mut TestApp<BackupsShell>) -> Vec<(String, Option<usize>)> {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .filter(|(_, node)| node.role() == Role::TreeItem)
        .filter_map(|(_, node)| Some((node.label()?.to_owned(), node.level())))
        .collect()
}

#[test]
fn the_snapshot_lists_its_files_under_their_folders_and_folds_them_away() {
    let mut app = TestApp::<BackupsShell>::capture(Some("restore")).unwrap();
    let listed = rows(&mut app);
    assert!(
        listed.iter().any(|(_, level)| *level == Some(1)),
        "the index has a top row: {listed:?}"
    );
    assert!(
        listed.iter().any(|(_, level)| *level == Some(2)),
        "a folder's files stand under it: {listed:?}"
    );
    let targets = app.harness().targets();
    let list = targets
        .iter()
        .find(|(name, _)| name.ends_with("/Section[0]/VirtualList"))
        .unwrap_or_else(|| panic!("the contents list stands in the sheet"))
        .1;
    let row = targets
        .iter()
        .find(|(name, _)| name.ends_with("/VirtualList[0]/ListRow[0]"))
        .unwrap_or_else(|| panic!("the contents list has a first row"))
        .1;
    let disclosure = (
        f64::midpoint(list.x0, row.x0),
        f64::midpoint(row.y0, row.y1),
    );
    let shown = listed.len();
    app.harness().click_at(disclosure.0, disclosure.1);
    let folded = rows(&mut app);
    assert!(folded.len() < shown, "folding hides the files: {folded:?}");
    assert!(
        folded.iter().all(|(_, level)| *level == Some(1)),
        "only the top rows stand: {folded:?}"
    );
    app.harness().click_at(disclosure.0, disclosure.1);
    assert_eq!(rows(&mut app).len(), shown, "the folder opens again");
}
